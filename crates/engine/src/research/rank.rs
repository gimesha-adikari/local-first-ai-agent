//! Deterministic lexical relevance for web evidence. No embedding model is
//! required. Pages are split into bounded passages, ranked with BM25 against
//! the user's own question, and the best passages are kept in document order
//! within a per-source character budget.

use agent_core::{
    chat_context::search_terms,
    research::{ExtractedPage, TextBlock},
};

const PASSAGE: usize = 700;
const MAX_PASSAGES: usize = 2_000;
const K1: f64 = 1.2;
const B: f64 = 0.75;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Passage {
    heading: Option<String>,
    text: String,
}

/// Merge short blocks under one heading and split long ones near sentence
/// ends, so every passage is at most about [`PASSAGE`] characters.
pub(crate) fn passages(blocks: &[TextBlock]) -> Vec<Passage> {
    let mut out: Vec<Passage> = Vec::new();
    for block in blocks {
        for piece in split(&block.text) {
            match out.last_mut() {
                Some(last)
                    if last.heading == block.heading
                        && last.text.len() + piece.len() < PASSAGE / 2 =>
                {
                    last.text.push(' ');
                    last.text.push_str(piece);
                }
                _ => {
                    if out.len() == MAX_PASSAGES {
                        return out;
                    }
                    out.push(Passage {
                        heading: block.heading.clone(),
                        text: piece.to_owned(),
                    });
                }
            }
        }
    }
    out
}

fn split(text: &str) -> Vec<&str> {
    let mut pieces = Vec::new();
    let mut rest = text;
    while rest.len() > PASSAGE {
        let mut cut = PASSAGE;
        while !rest.is_char_boundary(cut) {
            cut -= 1;
        }
        // Prefer a sentence or clause end in the second half of the window.
        let window = &rest[..cut];
        if let Some(end) = window
            .char_indices()
            .filter(|(index, c)| {
                *index > cut / 2 && matches!(c, '.' | '!' | '?' | '。' | '！' | '？' | ';')
            })
            .map(|(index, c)| index + c.len_utf8())
            .next_back()
        {
            cut = end;
        }
        pieces.push(rest[..cut].trim());
        rest = rest[cut..].trim_start();
    }
    if !rest.is_empty() {
        pieces.push(rest);
    }
    pieces
}

/// Query terms from the user's question, lowercased.
pub(crate) fn terms(question: &str) -> Vec<String> {
    search_terms(question, 24)
}

fn count(haystack: &str, needle: &str) -> usize {
    if needle.is_empty() {
        0
    } else {
        haystack.matches(needle).count()
    }
}

/// Selected evidence from one source.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Selection {
    pub text: String,
    /// Fraction of distinct query terms present in the selected text.
    pub coverage: f64,
    /// No passage matched, so the page's opening text was used.
    pub lead: bool,
}

fn cut(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max.saturating_sub(3);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", text[..end].trim_end())
}

pub(crate) fn coverage(text: &str, terms: &[String]) -> f64 {
    if terms.is_empty() {
        return 0.0;
    }
    let text = text.to_lowercase();
    terms
        .iter()
        .filter(|term| text.contains(term.as_str()))
        .count() as f64
        / terms.len() as f64
}

/// Rank a page's passages and keep the best within `budget` characters.
pub(crate) fn select(page: &ExtractedPage, terms: &[String], budget: usize) -> Selection {
    let passages = passages(&page.blocks);
    let lowered: Vec<(String, String)> = passages
        .iter()
        .map(|passage| {
            (
                passage.text.to_lowercase(),
                passage
                    .heading
                    .as_deref()
                    .unwrap_or_default()
                    .to_lowercase(),
            )
        })
        .collect();
    let total = lowered.len().max(1) as f64;
    let average = lowered.iter().map(|(text, _)| text.len()).sum::<usize>() as f64 / total;
    let idf: Vec<f64> = terms
        .iter()
        .map(|term| {
            let df = lowered
                .iter()
                .filter(|(text, heading)| {
                    text.contains(term.as_str()) || heading.contains(term.as_str())
                })
                .count() as f64;
            (1.0 + (total - df + 0.5) / (df + 0.5)).ln()
        })
        .collect();
    let mut scored: Vec<(usize, f64)> = lowered
        .iter()
        .enumerate()
        .map(|(index, (text, heading))| {
            let length = text.len() as f64 / average.max(1.0);
            let score = terms
                .iter()
                .zip(&idf)
                .map(|(term, idf)| {
                    let tf = count(text, term) as f64;
                    let body = idf * tf * (K1 + 1.0) / (tf + K1 * (1.0 - B + B * length));
                    let title = if heading.contains(term.as_str()) {
                        0.5 * idf
                    } else {
                        0.0
                    };
                    body + title
                })
                .sum::<f64>();
            (index, score)
        })
        .collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    let lead = scored.first().is_none_or(|(_, score)| *score <= 0.0);
    let mut chosen: Vec<usize> = Vec::new();
    let mut used = page
        .description
        .as_ref()
        .filter(|_| lead)
        .map_or(0, String::len);
    let order: Vec<usize> = if lead {
        (0..passages.len()).collect()
    } else {
        scored
            .iter()
            .filter(|(_, score)| *score > 0.0)
            .map(|(index, _)| *index)
            .collect()
    };
    for index in order {
        let length = passages[index].text.len() + 3;
        if used + length > budget && !chosen.is_empty() {
            continue;
        }
        chosen.push(index);
        used += length;
        if used >= budget {
            break;
        }
    }
    chosen.sort_unstable();
    let mut parts: Vec<String> = Vec::new();
    if lead && let Some(description) = &page.description {
        parts.push(description.clone());
    }
    parts.extend(chosen.into_iter().map(|index| passages[index].text.clone()));
    let text = cut(&parts.join("\n\n"), budget);
    Selection {
        coverage: coverage(&text, terms),
        text,
        lead,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(heading: Option<&str>, text: &str) -> TextBlock {
        TextBlock {
            heading: heading.map(str::to_owned),
            text: text.to_owned(),
        }
    }

    #[test]
    fn ranks_relevant_passages_and_keeps_document_order() {
        let page = ExtractedPage {
            blocks: vec![
                block(
                    None,
                    &"Unrelated introduction about gardening and soil. ".repeat(12),
                ),
                block(
                    Some("Opening"),
                    "The Fernhill bridge opened to traffic in March 2026.",
                ),
                block(
                    None,
                    &"More gardening advice about tomatoes and compost. ".repeat(12),
                ),
                block(
                    Some("Length"),
                    "The bridge spans 120 metres across the river.",
                ),
            ],
            ..Default::default()
        };
        let terms = terms("When did the Fernhill bridge open?");
        assert_eq!(terms, ["fernhill", "bridge", "open"]);
        let selection = select(&page, &terms, 200);
        assert!(!selection.lead);
        assert!(
            selection.text.starts_with("The Fernhill bridge opened"),
            "{}",
            selection.text
        );
        assert!(!selection.text.contains("gardening"));
        assert!(selection.text.len() <= 200);
        assert_eq!(selection.coverage, 1.0);
        // Deterministic: the same input selects the same evidence.
        assert_eq!(select(&page, &terms, 200), selection);
    }

    #[test]
    fn falls_back_to_the_page_opening_and_splits_long_blocks() {
        let page = ExtractedPage {
            description: Some("Summary line.".into()),
            blocks: vec![block(None, &"Sentence one is here. ".repeat(80))],
            ..Default::default()
        };
        let selection = select(&page, &terms("summarize"), 400);
        assert!(selection.lead);
        assert!(selection.text.starts_with("Summary line.\n\nSentence one"));
        assert!(selection.text.chars().count() <= 400);
        assert!(
            passages(&page.blocks)
                .iter()
                .all(|passage| passage.text.len() <= PASSAGE)
        );
        let cjk = ExtractedPage {
            blocks: vec![block(None, &"東京の橋は長い。".repeat(200))],
            ..Default::default()
        };
        assert!(passages(&cjk.blocks).len() > 1);
    }
}
