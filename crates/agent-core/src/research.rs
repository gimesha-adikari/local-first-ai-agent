//! Versioned web research contracts. Search providers and public webpage
//! retrieval are independent capabilities: a search adapter returns
//! normalized results, a page adapter returns bounded extracted evidence, and
//! the engine assembles, fits and cites that evidence for any supported
//! intelligence provider. Nothing here performs I/O.

use serde::{Deserialize, Serialize};

use crate::{
    capability::{Capability, CapabilityId, WEB_PAGE, WEB_SEARCH},
    chat_context::MAX_CHAT_SOURCES,
    inference::{ErrorCode, InferenceError, Result},
};

/// Longest search query sent to a provider, in characters. Brave documents
/// 600 characters and 75 words as its limit; SearXNG sets none. Kept
/// conservative.
pub const MAX_QUERY_CHARS: usize = 400;
/// Most words in one search query.
pub const MAX_QUERY_WORDS: usize = 50;
/// Results requested from a search provider.
pub const SEARCH_RESULTS: usize = 8;
/// Results kept after normalization and de-duplication.
pub const MAX_SEARCH_RESULTS: usize = 10;
/// Public pages retrieved for one research request.
pub const MAX_PAGES: usize = 3;
/// Sources offered to the model. Citations use the ordinary `[S1]` slots.
pub const MAX_WEB_SOURCES: usize = MAX_CHAT_SOURCES;
/// Largest webpage body read, before decoding.
pub const MAX_PAGE_BYTES: usize = 2 * 1024 * 1024;
/// Largest search-provider response read.
pub const MAX_SEARCH_BYTES: usize = 1024 * 1024;
/// Most characters of readable text kept from one page.
pub const MAX_PAGE_TEXT: usize = 120_000;
/// Most characters of evidence supplied to the model from one source.
pub const MAX_EVIDENCE_CHARS: usize = 1_600;
/// Redirects followed for one page; each hop is authorized on its own.
pub const MAX_REDIRECTS: usize = 4;
/// Longest public URL accepted.
pub const MAX_URL_CHARS: usize = 2_048;
/// Longest title or snippet kept from a search result.
pub const MAX_RESULT_TITLE: usize = 300;
pub const MAX_RESULT_SNIPPET: usize = 1_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchProviderKind {
    /// The Brave Search API, with the user's own subscription key.
    Brave,
    /// A SearXNG instance the user configured, with JSON output enabled.
    Searxng,
    /// Experimental: Purili's keyless public search API. Its terms limit it
    /// to personal, non-commercial use, which the user must confirm.
    Purili,
}

impl SearchProviderKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Brave => "Brave Search",
            Self::Searxng => "SearXNG",
            Self::Purili => "Purili",
        }
    }
}

/// Search provider configuration. No credential is ever part of it: the
/// Brave key is entered natively and kept in secure storage.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebResearchSettings {
    /// The provider used for web search. `None` leaves only direct page
    /// reading, which needs no search provider.
    #[serde(default)]
    pub provider: Option<SearchProviderKind>,
    /// The user's Brave plan permits storing results, so cited links may be
    /// saved with the conversation. Off by default.
    #[serde(default)]
    pub brave_save_results: bool,
    /// The SearXNG base URL, empty when not configured. Never a public
    /// instance chosen by the application.
    #[serde(default)]
    pub searxng_endpoint: String,
    /// Save cited links found through SearXNG with the conversation.
    #[serde(default)]
    pub searxng_save_results: bool,
    /// The user confirmed that their use of Purili is personal and
    /// non-commercial. Required to select it. Off by default.
    #[serde(default)]
    pub purili_personal_use: bool,
}

impl WebResearchSettings {
    pub fn validate(&self) -> Result<()> {
        if self.searxng_endpoint.len() > 512 || self.searxng_endpoint.chars().any(char::is_control)
        {
            return Err(invalid("Enter a SearXNG URL of at most 512 characters."));
        }
        if self.provider == Some(SearchProviderKind::Searxng)
            && self.searxng_endpoint.trim().is_empty()
        {
            return Err(invalid(
                "Enter your SearXNG instance URL to use it for search.",
            ));
        }
        if self.provider == Some(SearchProviderKind::Purili) && !self.purili_personal_use {
            return Err(invalid(
                "Purili allows only personal, non-commercial use. Confirm that below to select it.",
            ));
        }
        Ok(())
    }

    /// Whether search-result links from `provider` may be saved.
    pub fn may_save(&self, provider: SearchProviderKind) -> bool {
        match provider {
            SearchProviderKind::Brave => self.brave_save_results,
            SearchProviderKind::Searxng => self.searxng_save_results,
            // Its published pages state no storage permission: transient only.
            SearchProviderKind::Purili => false,
        }
    }
}

/// A bounded query. Chat uses a model-generated query for typed searches
/// and the explicitly reviewed text for visual searches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchQuery(String);

impl SearchQuery {
    pub fn new(text: &str) -> Result<Self> {
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if text.is_empty() {
            return Err(invalid("Enter something to search for."));
        }
        if text.chars().count() > MAX_QUERY_CHARS
            || text.split(' ').count() > MAX_QUERY_WORDS
            || text.chars().any(char::is_control)
        {
            return Err(invalid(&format!(
                "Shorten the search to at most {MAX_QUERY_CHARS} characters and {MAX_QUERY_WORDS} words."
            )));
        }
        Ok(Self(text))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn invalid(message: &str) -> InferenceError {
    InferenceError::new(ErrorCode::InvalidConfig, message)
}

/// One normalized search result. Every field is untrusted provider data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResult {
    pub title: String,
    /// Canonical HTTPS URL, validated by the engine before it is kept.
    pub url: String,
    pub snippet: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub published: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub site: Option<String>,
}

/// How a source's evidence was obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    /// Text extracted from the retrieved page itself.
    Page,
    /// Only the search provider's snippet; the page was not read.
    Snippet,
}

/// One block of readable page text with its nearest heading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextBlock {
    pub heading: Option<String>,
    pub text: String,
}

/// Bounded readable content extracted from one public page.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtractedPage {
    pub title: Option<String>,
    pub description: Option<String>,
    pub published: Option<String>,
    pub language: Option<String>,
    pub blocks: Vec<TextBlock>,
    /// Text was cut at [`MAX_PAGE_TEXT`].
    pub truncated: bool,
    /// Little readable text next to scripts: the page likely needs
    /// JavaScript, which is never run.
    pub needs_scripts: bool,
}

impl ExtractedPage {
    pub fn text_len(&self) -> usize {
        self.blocks.iter().map(|block| block.text.len()).sum()
    }
}

pub struct WebSearch;

impl Capability for WebSearch {
    const ID: CapabilityId = WEB_SEARCH;
    type Input = SearchQuery;
    type Output = Vec<SearchResult>;
    type Delta = std::convert::Infallible;
}

pub struct WebPage;

impl Capability for WebPage {
    const ID: CapabilityId = WEB_PAGE;
    /// A canonical public HTTPS URL.
    type Input = String;
    type Output = ExtractedPage;
    type Delta = std::convert::Infallible;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResearchMode {
    /// Search with the configured provider, then read the top public pages.
    Search,
    /// Read one public webpage the user supplied. No search provider.
    Page,
}

/// Where a research answer was generated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Synthesis {
    Local,
    Remote,
    /// No written answer was produced.
    None,
}

/// Persisted metadata for one source in citation-slot order. Only fields the
/// applicable provider terms allow are kept; the rest stay transient.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebSourceRecord {
    pub evidence: EvidenceKind,
    /// Whether link details were saved with the conversation.
    pub saved: bool,
    /// Canonical URL without its query string or fragment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// The retrieved URL had a query string that was not saved.
    #[serde(default)]
    pub query_removed: bool,
}

/// What a research attempt did, saved with the assistant message in the
/// encrypted profile. It never holds page text, snippets or raw results.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResearchRecord {
    pub version: u32,
    pub mode: ResearchMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<SearchProviderKind>,
    pub synthesis: Synthesis,
    /// Results returned by the search provider across the attempts.
    pub discovered: u32,
    /// Pages fetched and extracted successfully.
    pub retrieved: u32,
    /// Pages attempted that could not be read.
    pub unavailable: u32,
    /// Candidate sources in citation-slot order. The fitted context records
    /// how many of them were actually supplied to the model.
    pub sources: Vec<WebSourceRecord>,
    /// Set when search results or sources were kept transient only.
    #[serde(default)]
    pub transient_results: bool,
}

/// Progress of a running research turn, streamed to the surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResearchStage {
    Understanding,
    Searching,
    Checking,
    Refining,
    Reading,
    Ranking,
    Answering,
    Done,
}

/// Strip ASCII and Unicode controls, collapse whitespace and cut to `max`
/// characters on a character boundary.
pub fn clean_text(value: &str, max: usize) -> String {
    let mut out = String::new();
    let mut space = false;
    for c in value.chars() {
        if c.is_whitespace() || c.is_control() || is_format(c) {
            space = !out.is_empty();
            continue;
        }
        if space {
            out.push(' ');
            space = false;
        }
        out.push(c);
        if out.len() >= max * 4 {
            break;
        }
    }
    match out.char_indices().nth(max) {
        Some((index, _)) => {
            out.truncate(index);
            out.push('…');
            out
        }
        None => out,
    }
}

/// Bidirectional overrides and zero-width characters can disguise text.
fn is_format(c: char) -> bool {
    matches!(c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{2069}' | '\u{feff}')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queries_are_normalized_and_bounded() {
        assert_eq!(
            SearchQuery::new("  rust \n 1.100   release ")
                .unwrap()
                .as_str(),
            "rust 1.100 release"
        );
        assert!(SearchQuery::new("   ").is_err());
        assert!(SearchQuery::new(&"a".repeat(MAX_QUERY_CHARS + 1)).is_err());
        assert!(SearchQuery::new(&"w ".repeat(MAX_QUERY_WORDS + 1)).is_err());
        assert!(SearchQuery::new(&"é".repeat(MAX_QUERY_CHARS)).is_ok());
    }

    #[test]
    fn purili_needs_the_personal_use_confirmation_and_is_never_saved() {
        let mut settings: WebResearchSettings =
            serde_json::from_str(r#"{"provider":"purili"}"#).unwrap();
        assert!(settings.validate().is_err());
        settings.purili_personal_use = true;
        settings.validate().unwrap();
        settings.brave_save_results = true;
        settings.searxng_save_results = true;
        assert!(!settings.may_save(SearchProviderKind::Purili));
        // Settings saved before Purili existed stay unconfirmed.
        let earlier: WebResearchSettings =
            serde_json::from_str(r#"{"provider":"brave","braveSaveResults":false}"#).unwrap();
        assert!(!earlier.purili_personal_use);
    }

    #[test]
    fn clean_text_removes_controls_and_disguises() {
        assert_eq!(clean_text("a\u{202e}b\u{200b}c\n\td", 100), "a b c d");
        assert_eq!(clean_text("日本語のテキスト", 3), "日本語…");
        assert_eq!(clean_text("  ", 10), "");
    }

    #[test]
    fn records_round_trip_without_content_fields() {
        let record = ResearchRecord {
            version: 1,
            mode: ResearchMode::Search,
            provider: Some(SearchProviderKind::Brave),
            synthesis: Synthesis::Local,
            discovered: 5,
            retrieved: 2,
            unavailable: 1,
            sources: vec![WebSourceRecord {
                evidence: EvidenceKind::Page,
                saved: false,
                url: None,
                title: None,
                host: None,
                query_removed: false,
            }],
            transient_results: true,
        };
        let json = serde_json::to_string(&record).unwrap();
        assert!(!json.contains("snippet") && !json.contains("\"url\""));
        assert_eq!(
            serde_json::from_str::<ResearchRecord>(&json).unwrap(),
            record
        );
    }
}
