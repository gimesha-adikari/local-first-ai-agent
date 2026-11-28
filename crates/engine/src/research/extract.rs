//! Readable text extraction from untrusted HTML. The maintained html5ever
//! tokenizer drives a small streaming state machine: no DOM is built, no
//! script runs, no subresource is requested and nothing is rendered. Script,
//! style and other active or decorative content is discarded. Input, text,
//! blocks and nesting are bounded.

use std::cell::RefCell;

use agent_core::research::{ExtractedPage, MAX_PAGE_BYTES, MAX_PAGE_TEXT, TextBlock, clean_text};
use html5ever::{
    tendril::StrTendril,
    tokenizer::{
        BufferQueue, CharacterTokens, EndTag, StartTag, Tag, TagToken, Token, TokenSink,
        TokenSinkResult, Tokenizer, TokenizerOpts, states::RawKind,
    },
};

const MAX_BLOCKS: usize = 4_000;
const MAX_BLOCK: usize = 8_000;
const MAX_HEADING: usize = 200;
const MAX_META: usize = 500;
/// Main or article content shorter than this is not preferred over the page.
const PRIMARY_MIN: usize = 200;
/// Below this much readable text a page with scripts is reported as
/// script-dependent.
const SCRIPT_MIN: usize = 50;

/// Elements whose text is never page content.
const DISCARDED: &[&str] = &[
    "script", "style", "noscript", "template", "svg", "math", "iframe", "object", "embed",
    "canvas", "video", "audio", "picture", "map", "form", "button", "select", "textarea", "dialog",
    "nav", "aside", "footer", "menu",
];

/// Elements that end a block of text.
const BLOCKS: &[&str] = &[
    "p",
    "div",
    "li",
    "ul",
    "ol",
    "dl",
    "dt",
    "dd",
    "section",
    "article",
    "main",
    "header",
    "blockquote",
    "pre",
    "table",
    "tr",
    "td",
    "th",
    "figcaption",
    "figure",
    "br",
    "hr",
    "address",
    "details",
    "summary",
    "body",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
];

#[derive(Default)]
struct State {
    discard: Vec<&'static str>,
    /// Depth inside `<main>` or `<article>`.
    primary: usize,
    /// Depth inside a `<header>` outside the primary content.
    banner: usize,
    in_title: bool,
    heading: Option<String>,
    in_heading: bool,
    buffer: String,
    title: String,
    description: Option<String>,
    og_title: Option<String>,
    published: Option<String>,
    language: Option<String>,
    blocks: Vec<(TextBlock, bool)>,
    text_len: usize,
    truncated: bool,
    scripts: usize,
}

impl State {
    fn flush(&mut self) {
        if self.buffer.is_empty() {
            return;
        }
        let raw = std::mem::take(&mut self.buffer);
        if raw.len() >= 4 * MAX_BLOCK || raw.chars().count() > MAX_BLOCK {
            self.truncated = true;
        }
        let text = clean_text(&raw, MAX_BLOCK);
        if text.is_empty() {
            return;
        }
        if self.in_heading {
            self.heading = Some(clean_text(&text, MAX_HEADING));
        }
        if self.blocks.len() >= MAX_BLOCKS || self.text_len >= MAX_PAGE_TEXT {
            self.truncated = true;
            return;
        }
        let room = MAX_PAGE_TEXT - self.text_len;
        let text = if text.len() > room {
            self.truncated = true;
            let mut end = room;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text[..end].to_owned()
        } else {
            text
        };
        self.text_len += text.len();
        let heading = if self.in_heading {
            None
        } else {
            self.heading.clone()
        };
        self.blocks
            .push((TextBlock { heading, text }, self.primary > 0));
    }

    fn start(&mut self, tag: &Tag) -> TokenSinkResult<()> {
        let name: &str = &tag.name;
        if name == "script" {
            self.scripts += 1;
        }
        match name {
            "html" => {
                if let Some(lang) = attribute(tag, "lang") {
                    self.language = Some(clean_text(lang, 35));
                }
            }
            "meta" => self.meta(tag),
            "time" if self.published.is_none() => {
                if let Some(value) = attribute(tag, "datetime") {
                    self.published = Some(clean_text(value, 64));
                }
            }
            _ => {}
        }
        if let Some(element) = DISCARDED.iter().find(|element| **element == name) {
            if !tag.self_closing && self.discard.len() < 64 {
                self.flush();
                self.discard.push(element);
            }
        } else if name == "title" && self.title.is_empty() {
            self.in_title = true;
        } else if BLOCKS.contains(&name) {
            self.flush();
            match name {
                "main" | "article" => self.primary += 1,
                "header" if self.primary == 0 => self.banner += 1,
                "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => self.in_heading = true,
                _ => {}
            }
        }
        // Raw text elements must not be tokenized as markup.
        match name {
            "script" => TokenSinkResult::RawData(RawKind::ScriptData),
            "style" | "xmp" | "iframe" | "noembed" | "noframes" | "noscript" => {
                TokenSinkResult::RawData(RawKind::Rawtext)
            }
            "title" | "textarea" => TokenSinkResult::RawData(RawKind::Rcdata),
            "plaintext" => TokenSinkResult::Plaintext,
            _ => TokenSinkResult::Continue,
        }
    }

    fn end(&mut self, tag: &Tag) {
        let name: &str = &tag.name;
        if let Some(position) = self.discard.iter().rposition(|element| *element == name) {
            self.discard.truncate(position);
            self.buffer.clear();
            return;
        }
        if name == "title" {
            self.in_title = false;
        } else if BLOCKS.contains(&name) {
            self.flush();
            match name {
                "main" | "article" => self.primary = self.primary.saturating_sub(1),
                "header" if self.primary == 0 => self.banner = self.banner.saturating_sub(1),
                "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => self.in_heading = false,
                _ => {}
            }
        }
    }

    fn text(&mut self, text: &str) {
        if self.in_title {
            if self.title.len() < 4 * MAX_HEADING {
                self.title.push_str(text);
            }
            return;
        }
        if !self.discard.is_empty() || self.banner > 0 {
            return;
        }
        if self.buffer.len() < 4 * MAX_BLOCK {
            self.buffer.push_str(text);
        }
    }

    fn meta(&mut self, tag: &Tag) {
        let key = attribute(tag, "name")
            .or_else(|| attribute(tag, "property"))
            .or_else(|| attribute(tag, "itemprop"))
            .map(str::to_ascii_lowercase);
        let Some(content) = attribute(tag, "content") else {
            return;
        };
        let value = || Some(clean_text(content, MAX_META)).filter(|value| !value.is_empty());
        match key.as_deref() {
            Some("description" | "og:description") if self.description.is_none() => {
                self.description = value();
            }
            Some("og:title") if self.og_title.is_none() => self.og_title = value(),
            Some("article:published_time" | "datepublished" | "date" | "dc.date" | "pubdate")
                if self.published.is_none() =>
            {
                self.published = value().map(|value| clean_text(&value, 64));
            }
            _ => {}
        }
    }
}

fn attribute<'a>(tag: &'a Tag, name: &str) -> Option<&'a str> {
    tag.attrs
        .iter()
        .find(|attr| &*attr.name.local == name)
        .map(|attr| &*attr.value)
}

struct Sink(RefCell<State>);

impl TokenSink for Sink {
    type Handle = ();

    fn process_token(&self, token: Token, _line: u64) -> TokenSinkResult<()> {
        let mut state = self.0.borrow_mut();
        match token {
            TagToken(tag) => match tag.kind {
                StartTag => state.start(&tag),
                EndTag => {
                    state.end(&tag);
                    TokenSinkResult::Continue
                }
            },
            CharacterTokens(text) => {
                state.text(&text);
                TokenSinkResult::Continue
            }
            _ => TokenSinkResult::Continue,
        }
    }
}

/// Decode a page body. The declared charset wins, then a byte-order mark,
/// then an early `<meta charset>`, and UTF-8 otherwise. Malformed sequences
/// become U+FFFD; nothing is guessed from content.
pub(crate) fn decode(body: &[u8], content_type: &str) -> String {
    let body = &body[..body.len().min(MAX_PAGE_BYTES)];
    let declared = content_type
        .split(';')
        .skip(1)
        .find_map(|part| {
            let (name, value) = part.split_once('=')?;
            name.trim()
                .eq_ignore_ascii_case("charset")
                .then(|| value.trim().trim_matches('"'))
        })
        .and_then(|label| encoding_rs::Encoding::for_label(label.as_bytes()))
        .or_else(|| meta_charset(&body[..body.len().min(1024)]))
        .map(|encoding| encoding.output_encoding())
        .unwrap_or(encoding_rs::UTF_8);
    declared.decode(body).0.into_owned()
}

fn meta_charset(head: &[u8]) -> Option<&'static encoding_rs::Encoding> {
    let text = String::from_utf8_lossy(head).to_ascii_lowercase();
    let start = text.find("charset=")? + "charset=".len();
    let label: String = text[start..]
        .trim_start_matches(['"', '\''])
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':' | '.'))
        .collect();
    encoding_rs::Encoding::for_label(label.as_bytes())
}

/// Extract readable content from a decoded HTML document.
pub(crate) fn html(document: &str) -> ExtractedPage {
    let sink = Sink(RefCell::new(State::default()));
    let tokenizer = Tokenizer::new(sink, TokenizerOpts::default());
    let queue = BufferQueue::default();
    queue.push_back(StrTendril::from_slice(document));
    let _ = tokenizer.feed(&queue);
    tokenizer.end();
    let mut state = tokenizer.sink.0.into_inner();
    state.flush();
    // Prefer the page's main or article content when it has substance.
    let primary: usize = state
        .blocks
        .iter()
        .filter(|(_, primary)| *primary)
        .map(|(block, _)| block.text.len())
        .sum();
    let blocks: Vec<TextBlock> = state
        .blocks
        .into_iter()
        .filter(|(_, in_primary)| primary < PRIMARY_MIN || *in_primary)
        .map(|(block, _)| block)
        .collect();
    let title = Some(clean_text(&state.title, MAX_HEADING))
        .filter(|title| !title.is_empty())
        .or(state.og_title);
    let text: usize = blocks.iter().map(|block| block.text.len()).sum();
    ExtractedPage {
        title,
        description: state.description,
        published: state.published,
        language: state.language,
        needs_scripts: text < SCRIPT_MIN && state.scripts > 0,
        blocks,
        truncated: state.truncated,
    }
}

/// Plain-text pages: paragraphs separated by blank lines.
pub(crate) fn plain(document: &str) -> ExtractedPage {
    let mut page = ExtractedPage::default();
    let mut total = 0;
    for paragraph in document.split("\n\n") {
        let text = clean_text(paragraph, MAX_BLOCK);
        if text.is_empty() {
            continue;
        }
        if total + text.len() > MAX_PAGE_TEXT || page.blocks.len() >= MAX_BLOCKS {
            page.truncated = true;
            break;
        }
        total += text.len();
        page.blocks.push(TextBlock {
            heading: None,
            text,
        });
    }
    page
}

/// Decode and extract a fetched page by its declared media type.
pub(crate) fn page(body: &[u8], content_type: &str) -> ExtractedPage {
    let document = decode(body, content_type);
    if content_type.starts_with("text/plain") {
        plain(&document)
    } else {
        html(&document)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_readable_text_and_drops_active_and_decorative_content() {
        let page = html(concat!(
            "<!doctype html><html lang='de'><head><title>Synthetic &amp; Title</title>",
            "<meta name='description' content='A synthetic page.'>",
            "<meta property='article:published_time' content='2026-11-01T08:00:00Z'>",
            "<style>body{color:red} .x::after{content:'STYLE-LEAK'}</style>",
            "<script>document.write('SCRIPT-LEAK </p>');</script></head><body>",
            "<header><nav>Home | NAV-LEAK</nav>Site banner BANNER-LEAK</header>",
            "<main><h1>Bridge report</h1><p>The bridge opened in <b>2026</b>.</p>",
            "<form><input value='FORM-LEAK'><button>BUTTON-LEAK</button></form>",
            "<p>Größe: 120 m. 橋の長さは120メートルです。</p>",
            "<iframe src='https://tracker.invalid/'>IFRAME-LEAK</iframe>",
            "<noscript>NOSCRIPT-LEAK</noscript><svg><text>SVG-LEAK</text></svg>",
            "<img src='https://img.invalid/x.png' alt='ALT-LEAK'></main>",
            "<aside>ASIDE-LEAK</aside><footer>FOOTER-LEAK</footer></body></html>"
        ));
        assert_eq!(page.title.as_deref(), Some("Synthetic & Title"));
        assert_eq!(page.description.as_deref(), Some("A synthetic page."));
        assert_eq!(page.published.as_deref(), Some("2026-11-01T08:00:00Z"));
        assert_eq!(page.language.as_deref(), Some("de"));
        let text: Vec<&str> = page
            .blocks
            .iter()
            .map(|block| block.text.as_str())
            .collect();
        assert_eq!(
            text,
            [
                "Bridge report",
                "The bridge opened in 2026.",
                "Größe: 120 m. 橋の長さは120メートルです。"
            ]
        );
        assert_eq!(page.blocks[1].heading.as_deref(), Some("Bridge report"));
        let all = format!("{page:?}");
        assert!(!all.contains("LEAK"), "{all}");
        assert!(!page.needs_scripts);
    }

    #[test]
    fn reports_script_dependent_pages_and_bounds_text() {
        let shell =
            html("<html><body><div id=app></div><script src=app.js></script></body></html>");
        assert!(shell.needs_scripts);
        assert!(shell.blocks.is_empty());
        let huge = format!("<p>{}</p>", "word ".repeat(1_000)).repeat(100);
        let page = html(&huge);
        assert!(page.truncated);
        assert!(page.text_len() <= MAX_PAGE_TEXT);
        // Unclosed and deeply nested markup stays bounded.
        let nested = "<div>".repeat(10_000) + "deep text" + &"<aside>".repeat(100);
        assert_eq!(html(&nested).blocks[0].text, "deep text");
    }

    #[test]
    fn decodes_declared_and_meta_charsets() {
        let latin = b"<p>Caf\xe9 cr\xe8me</p>";
        assert_eq!(
            decode(latin, "text/html; charset=ISO-8859-1"),
            "<p>Café crème</p>"
        );
        let mut meta = b"<meta charset=\"shift_jis\"><p>".to_vec();
        meta.extend_from_slice(&[0x93, 0xfa, 0x96, 0x7b]);
        assert!(decode(&meta, "text/html").contains("日本"));
        assert_eq!(decode("<p>ok ✓</p>".as_bytes(), "text/html"), "<p>ok ✓</p>");
        assert_eq!(decode(b"a\xffb", "text/html; charset=utf-8"), "a\u{fffd}b");
        let text = page(b"First para.\n\nSecond\npara.", "text/plain; charset=utf-8");
        assert_eq!(text.blocks.len(), 2);
        assert_eq!(text.blocks[1].text, "Second para.");
    }
}
