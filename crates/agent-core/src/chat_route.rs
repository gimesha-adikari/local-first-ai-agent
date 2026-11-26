//! Bounded intent routing for Chat requests with selected documents.
//!
//! The router combines a few signals instead of matching whole phrases:
//! small cue lexicons (search verbs, summary words, creative and social
//! requests), the number of content terms left after removing function words
//! and document references, references to the selected documents, and the
//! previous document question for short follow-ups. The host then checks
//! retrieval relevance; a `Question` with no qualifying passage becomes
//! `General` under automatic grounding.

use crate::chat_context::{ChatIntent, Grounding, search_terms};

/// Words considered per request. Longer requests are routed on this prefix.
const MAX_ROUTED_WORDS: usize = 64;
/// Upper bound on a follow-up that may inherit the previous document question.
const MAX_FOLLOW_UP_WORDS: usize = 8;
const MAX_SEARCH_TARGET_BYTES: usize = 128;

const SEARCH_VERBS: &[&str] = &["find", "search", "locate", "grep", "lookup"];
const DOCUMENT_NOUNS: &[&str] = &[
    "document",
    "documents",
    "doc",
    "docs",
    "file",
    "files",
    "pdf",
    "pdfs",
    "selected",
    "attachment",
    "attachments",
    "attached",
];
const PRONOUNS: &[&str] = &[
    "it", "its", "this", "that", "these", "those", "they", "them", "there",
];
const OVERVIEW_CUES: &[&str] = &[
    "summarize",
    "summarise",
    "summary",
    "overview",
    "outline",
    "gist",
    "tldr",
    "contents",
    "contain",
    "contains",
];
/// Words that describe the request rather than its subject.
const GENERIC: &[&str] = &[
    "about",
    "again",
    "all",
    "also",
    "anything",
    "brief",
    "briefly",
    "contain",
    "contained",
    "containing",
    "contains",
    "content",
    "contents",
    "cover",
    "covers",
    "describe",
    "description",
    "discuss",
    "discusses",
    "entire",
    "everything",
    "explain",
    "gist",
    "give",
    "go",
    "idea",
    "ideas",
    "include",
    "includes",
    "inside",
    "just",
    "key",
    "kind",
    "main",
    "mean",
    "means",
    "more",
    "now",
    "outline",
    "overview",
    "point",
    "points",
    "provide",
    "purpose",
    "quick",
    "read",
    "say",
    "says",
    "short",
    "show",
    "shows",
    "so",
    "sort",
    "summarise",
    "summarize",
    "summary",
    "talk",
    "talks",
    "through",
    "tldr",
    "topic",
    "topics",
    "type",
    "whole",
    "used",
    "use",
    "here",
];
const CREATIVE_OR_SOCIAL: &[&str] = &[
    "write",
    "compose",
    "poem",
    "poems",
    "story",
    "stories",
    "joke",
    "jokes",
    "haiku",
    "song",
    "lyrics",
    "limerick",
    "rhyme",
    "hello",
    "hi",
    "hey",
    "thanks",
    "thank",
    "translate",
];
/// Words between a search verb and its target.
const SEARCH_FILLER: &[&str] = &[
    "for",
    "up",
    "the",
    "a",
    "an",
    "word",
    "words",
    "term",
    "terms",
    "phrase",
    "text",
    "string",
    "occurrences",
    "occurrence",
    "mentions",
    "mention",
    "instances",
    "of",
    "all",
    "every",
    "me",
    "please",
    "where",
];
const COMMAND_LEAD_IN: &[&str] = &[
    "please", "can", "could", "would", "you", "help", "me", "i", "want", "to", "need", "let",
    "lets", "us", "now", "ok", "okay", "just", "also",
];
/// Words that ask to revise the previous answer rather than ask something new.
const REFINEMENTS: &[&str] = &[
    "shorter",
    "longer",
    "shorten",
    "simpler",
    "simplify",
    "expand",
    "elaborate",
    "rephrase",
    "reword",
    "rewrite",
    "bullet",
    "bullets",
    "concise",
    "briefer",
    "clarify",
    "detail",
    "details",
    "detailed",
    "more",
    "less",
    "again",
    "simply",
    "plain",
];
/// Words that tie a question to the selected documents.
const DOCUMENT_CUES: &[&str] = &[
    "according",
    "cite",
    "cited",
    "citation",
    "citations",
    "source",
    "sources",
];
const SEARCH_SCOPE: &[&str] = &["in", "within", "inside", "from", "across", "throughout"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutePlan {
    /// `Search` and `Overview` are final. `Question` becomes `General` under
    /// automatic grounding when retrieval finds no relevant passage.
    pub intent: ChatIntent,
    /// Literal text for `Search`, otherwise the text to rank passages with.
    pub query: String,
    /// The query includes the previous document question.
    pub follow_up: bool,
    /// The request explicitly refers to the selected documents. Under
    /// automatic grounding such a question without evidence gets the
    /// deterministic no-evidence answer instead of ordinary Chat.
    pub refers_to_documents: bool,
    /// A revision request ("make it shorter") applied to the previous
    /// document question, which is re-answered from current evidence.
    pub refinement: Option<String>,
}

fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .take(MAX_ROUTED_WORDS)
        .map(str::to_lowercase)
        .collect()
}

fn within_one_edit(word: &str, target: &str) -> bool {
    let (a, b): (Vec<char>, Vec<char>) = (word.chars().collect(), target.chars().collect());
    if a.len().abs_diff(b.len()) > 1 {
        return false;
    }
    let (mut i, mut j, mut edits) = (0, 0, 0);
    while i < a.len() && j < b.len() {
        if a[i] == b[j] {
            i += 1;
            j += 1;
            continue;
        }
        edits += 1;
        if edits > 1 {
            return false;
        }
        match a.len().cmp(&b.len()) {
            std::cmp::Ordering::Greater => i += 1,
            std::cmp::Ordering::Less => j += 1,
            std::cmp::Ordering::Equal => {
                i += 1;
                j += 1;
            }
        }
    }
    edits + (a.len() - i) + (b.len() - j) <= 1
}

fn quoted(text: &str) -> Option<&str> {
    for (open, close) in [('"', '"'), ('“', '”'), ('\'', '\''), ('‘', '’'), ('`', '`')] {
        if let Some(start) = text.find(open) {
            let rest = &text[start + open.len_utf8()..];
            if let Some(end) = rest.find(close) {
                let inner = rest[..end].trim();
                if inner.chars().any(char::is_alphanumeric) {
                    return Some(inner);
                }
            }
        }
    }
    None
}

fn bounded(text: &str) -> String {
    let end = text.floor_char_boundary(MAX_SEARCH_TARGET_BYTES);
    text[..end].trim().to_owned()
}

/// The literal text a search request asks for, when it has a search verb.
fn search_target(question: &str, words: &[String], documents_named: bool) -> Option<String> {
    let verb = words.iter().position(|word| {
        SEARCH_VERBS.contains(&word.as_str())
            // "look for" / "look up"
            || (word == "look")
            // A mistyped leading verb counts only when the request names the documents.
            || (documents_named
                && word.chars().count() >= 4
                && SEARCH_VERBS.iter().any(|verb| within_one_edit(word, verb)))
    })?;
    if words[verb] == "look"
        && !matches!(words.get(verb + 1).map(String::as_str), Some("for" | "up"))
    {
        return None;
    }
    // Only treat the verb as a command when nothing but a polite lead-in
    // precedes it, such as "please" or "can you".
    if !words[..verb]
        .iter()
        .all(|word| COMMAND_LEAD_IN.contains(&word.as_str()))
    {
        return None;
    }
    if let Some(literal) = quoted(question) {
        return Some(bounded(literal));
    }
    let mut target = Vec::new();
    for word in &words[verb + 1..] {
        if SEARCH_SCOPE.contains(&word.as_str()) && !target.is_empty() {
            break;
        }
        if target.is_empty() && SEARCH_FILLER.contains(&word.as_str()) {
            continue;
        }
        if DOCUMENT_NOUNS.contains(&word.as_str()) || PRONOUNS.contains(&word.as_str()) {
            if target.is_empty() {
                continue;
            }
            break;
        }
        target.push(word.as_str());
    }
    (!target.is_empty()).then(|| bounded(&target.join(" ")))
}

/// Route a request with selected documents. `previous` is the most recent
/// earlier user question that was answered from documents, if any.
pub fn route(question: &str, previous: Option<&str>, grounding: Grounding) -> RoutePlan {
    let words = words(question);
    let has = |list: &[&str]| words.iter().any(|word| list.contains(&word.as_str()));
    let documents_named = has(DOCUMENT_NOUNS);
    let pronoun = has(PRONOUNS) && words.len() <= MAX_FOLLOW_UP_WORDS;
    let content: Vec<String> = search_terms(question, 16)
        .into_iter()
        .filter(|term| {
            !GENERIC.contains(&term.as_str())
                && !PRONOUNS.contains(&term.as_str())
                && !DOCUMENT_NOUNS.contains(&term.as_str())
        })
        .collect();
    let refers_to_documents = documents_named || has(DOCUMENT_CUES);

    if let Some(target) = search_target(question, &words, documents_named) {
        return RoutePlan {
            intent: ChatIntent::Search,
            query: target,
            follow_up: false,
            refers_to_documents: true,
            refinement: None,
        };
    }
    // A short revision request after a document answer re-routes the previous
    // question; its evidence is retrieved again from the current selection.
    if let Some(previous) = previous.filter(|text| !text.trim().is_empty())
        && is_refinement(question)
    {
        let mut plan = route(previous, None, grounding);
        plan.follow_up = true;
        plan.refers_to_documents = true;
        plan.refinement = Some(question.trim().to_owned());
        return plan;
    }
    if content.is_empty() && (documents_named || pronoun || has(OVERVIEW_CUES)) {
        return RoutePlan {
            intent: ChatIntent::Overview,
            query: question.to_owned(),
            follow_up: false,
            refers_to_documents: true,
            refinement: None,
        };
    }
    // A creative or social request without a document reference never
    // inherits the previous document question.
    let creative = grounding == Grounding::Auto && has(CREATIVE_OR_SOCIAL) && !documents_named;
    let follow_up = !creative
        && words.len() <= MAX_FOLLOW_UP_WORDS
        && (pronoun || content.len() <= 1)
        && previous.is_some_and(|text| !text.trim().is_empty());
    let query = match previous {
        Some(previous) if follow_up => format!("{} {}", previous.trim(), question.trim()),
        _ => question.to_owned(),
    };
    RoutePlan {
        intent: ChatIntent::Question,
        query,
        follow_up,
        refers_to_documents: refers_to_documents || follow_up,
        refinement: None,
    }
}

/// A short request to revise the previous answer, with no new subject.
pub fn is_refinement(question: &str) -> bool {
    let words = words(question);
    words.len() <= MAX_FOLLOW_UP_WORDS
        && words
            .iter()
            .any(|word| REFINEMENTS.contains(&word.as_str()))
        && search_terms(question, 16).iter().all(|term| {
            REFINEMENTS.contains(&term.as_str())
                || GENERIC.contains(&term.as_str())
                || PRONOUNS.contains(&term.as_str())
                || matches!(
                    term.as_str(),
                    "make" | "bit" | "lot" | "much" | "little" | "points" | "point"
                )
        })
}

/// The text the model answers for a routed request. A refinement restates the
/// previous question so no earlier answer or evidence has to be reused.
pub fn routed_question(plan: &RoutePlan, question: &str, previous: Option<&str>) -> String {
    match (&plan.refinement, previous) {
        (Some(refinement), Some(previous)) => format!(
            "{}\n\nAnswer again from the current document evidence and follow this instruction: {}",
            previous.trim(),
            refinement
        ),
        _ => question.to_owned(),
    }
}

/// Whether a lexically ranked passage shares enough of the request's
/// significant terms to count as relevant: at least half, and at least one.
pub fn lexically_relevant(terms: &[String], text: &str) -> bool {
    if terms.is_empty() {
        return false;
    }
    let passage: std::collections::HashSet<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect();
    let matched = terms.iter().filter(|term| passage.contains(*term)).count();
    matched > 0 && matched * 2 >= terms.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn intent(question: &str) -> ChatIntent {
        route(question, None, Grounding::Auto).intent
    }

    #[test]
    fn manual_review_cases_route_to_the_expected_intent() {
        let search = route("fins run in this selected file", None, Grounding::Auto);
        assert_eq!(search.intent, ChatIntent::Search);
        assert_eq!(search.query, "run");
        assert_eq!(intent("what it contain"), ChatIntent::Overview);
        assert_eq!(intent("what it do"), ChatIntent::Overview);
        // Retrieval relevance decides; with no qualifying passage it becomes General.
        let poem = route(
            "Write a four-line poem about rain",
            Some("When does the orchard gate close?"),
            Grounding::Auto,
        );
        assert_eq!(poem.intent, ChatIntent::Question);
        assert!(!poem.follow_up);
        assert_eq!(poem.query, "Write a four-line poem about rain");
    }

    #[test]
    fn search_overview_question_and_follow_up_signals() {
        let quoted = route(
            "search for \"orchard gate\" in the pdf",
            None,
            Grounding::Auto,
        );
        assert_eq!(
            (quoted.intent, quoted.query.as_str()),
            (ChatIntent::Search, "orchard gate")
        );
        let plain = route(
            "Find invoice deadlines in this document",
            None,
            Grounding::Auto,
        );
        assert_eq!(plain.query, "invoice deadlines");
        assert_eq!(intent("look up warranty"), ChatIntent::Search);
        // A mistyped verb without a document reference is not a command.
        assert_eq!(intent("fine, tell me about trains"), ChatIntent::Question);
        // A late "find" is part of a question, not a command.
        assert_eq!(
            intent("Where can visitors find the orchard gate code?"),
            ChatIntent::Question
        );
        for overview in [
            "Summarize this document",
            "What is this file about?",
            "Give me an overview",
            "What does the PDF contain?",
            "summarize",
        ] {
            assert_eq!(intent(overview), ChatIntent::Overview, "{overview}");
        }
        for question in [
            "What does the PDF say about payments?",
            "Summarize the pros and cons of local AI models",
            "When does the orchard gate close?",
        ] {
            assert_eq!(intent(question), ChatIntent::Question, "{question}");
        }
        let follow = route(
            "and how do I start it?",
            Some("Which port does the backend use?"),
            Grounding::Auto,
        );
        assert!(follow.follow_up);
        assert!(follow.query.starts_with("Which port"));
        let standalone = route(
            "How often do commuter trains leave platform four?",
            Some("Which port does the backend use?"),
            Grounding::Auto,
        );
        assert!(!standalone.follow_up);
    }

    #[test]
    fn refinements_reroute_the_previous_document_question() {
        let previous = "Which port does the backend use?";
        for request in [
            "make it shorter",
            "explain more",
            "in bullet points please",
            "simpler",
        ] {
            let plan = route(request, Some(previous), Grounding::Auto);
            assert_eq!(plan.intent, ChatIntent::Question, "{request}");
            assert_eq!(plan.refinement.as_deref(), Some(request));
            assert!(routed_question(&plan, request, Some(previous)).starts_with(previous));
        }
        let overview = route("make it shorter", Some("what it contain"), Grounding::Auto);
        assert_eq!(overview.intent, ChatIntent::Overview);
        // Without a previous document question nothing is inherited.
        assert_eq!(
            route("make it shorter", None, Grounding::Auto).refinement,
            None
        );
        // A new subject is not a refinement.
        assert_eq!(
            route("more about parking fees", Some(previous), Grounding::Auto).refinement,
            None
        );
        assert!(
            route("What does the PDF say about fees?", None, Grounding::Auto).refers_to_documents
        );
        assert!(
            !route("Write a four-line poem about rain", None, Grounding::Auto).refers_to_documents
        );
    }

    #[test]
    fn lexical_relevance_needs_half_of_the_terms() {
        let terms = |text| search_terms(text, 16);
        assert!(lexically_relevant(
            &terms("When does the orchard gate close?"),
            "The orchard gate closes at seven."
        ));
        assert!(!lexically_relevant(
            &terms("Write a four-line poem about rain"),
            "Each line of the form must be signed."
        ));
        assert!(!lexically_relevant(&[], "anything"));
    }

    #[test]
    fn edit_distance_is_bounded_to_one() {
        assert!(within_one_edit("fins", "find"));
        assert!(within_one_edit("serch", "search"));
        assert!(!within_one_edit("fond", "search"));
        assert!(!within_one_edit("fixes", "find"));
    }
}
