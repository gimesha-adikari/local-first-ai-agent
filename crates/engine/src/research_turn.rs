//! The bounded single-pass research turn: an explicitly authorized query or
//! URL, at most one search, a few public page reads, deterministic evidence
//! selection, one grounded answer and deterministic citation validation.
//! There is no model-generated query, crawl or follow-up loop.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use agent_core::{
    chat::{ChatStore, ConversationId, GenerationState, Message, ModelId, TurnAction},
    chat_context::{AttemptContext, ChatContext, EvidenceTask, no_evidence_answer},
    classification::{Labels, PUBLIC_WEB, SYSTEM_TEXT, USER_INPUT},
    inference::{Cancellation, ErrorCode, InferenceError, Result},
    intelligence::{ContextSource, EgressRecord, SessionState, SourceKind},
    research::{
        EvidenceKind, ExtractedPage, MAX_EVIDENCE_CHARS, MAX_PAGES, MAX_REDIRECTS, MAX_WEB_SOURCES,
        ResearchMode, ResearchRecord, ResearchStage, SearchProviderKind, SearchQuery, SearchResult,
        Synthesis, WebSourceRecord, clean_text,
    },
};
use inference::Phase;

use super::{
    ChatEvent, ChatEvents, ChatOptions, ChatRuntime, StreamContext, check_chat_cancellation,
    context_candidates, deliver, remote,
};
use crate::{
    chat_session::ChatSession,
    database,
    network::{
        Failure,
        page::{PageError, PublicUrl, ResearchConsent},
    },
    research::{
        DiscoveredView, ResearchOptions, Transient, TransientSource, extract, local_only_error,
        page_limitation, rank,
    },
};

/// How long one research authorization stays usable.
const CONSENT_LIFETIME: Duration = Duration::from_secs(120);

/// One candidate source, in citation-slot order.
struct Candidate {
    evidence: EvidenceKind,
    url: PublicUrl,
    title: String,
    published: Option<String>,
    excerpt: String,
    labels: Labels,
    coverage: f64,
    rank: usize,
    partial: bool,
}

impl Candidate {
    /// The evidence line the model sees. It is quoted as untrusted data.
    fn evidence_text(&self) -> String {
        let kind = match (self.evidence, self.partial) {
            (EvidenceKind::Page, false) => "retrieved page",
            (EvidenceKind::Page, true) => "retrieved page, partial",
            (EvidenceKind::Snippet, _) => "search snippet only, page not read",
        };
        let published = self
            .published
            .as_deref()
            .map(|date| format!(" · published {date}"))
            .unwrap_or_default();
        format!(
            "{}\n{} · {kind}{published}\n{}",
            self.title,
            self.url.host(),
            self.excerpt
        )
    }
}

struct Target {
    synthesis: Synthesis,
    /// Why no model answers, for results-only turns.
    reason: Option<String>,
    model_name: Option<String>,
    context_tokens: u32,
}

impl ChatRuntime {
    fn research_event(&self, events: &ChatEvents, id: i64, stage: ResearchStage) {
        deliver(events, ChatEvent::Research { id, stage });
    }

    /// Decide where the answer is generated. There is no fallback between
    /// local and remote: a remote answer needs its own authorization.
    fn research_target(
        &self,
        research: &ResearchOptions,
        model_id: Option<ModelId>,
        thinking: bool,
    ) -> Result<(
        Target,
        Option<(
            storage::providers::SavedProvider,
            crate::network::Destination,
        )>,
    )> {
        let selection =
            crate::providers::effective_settings(&*database(&self.database)?, &self.policy)?;
        let remote = crate::providers::remote_target(&self.database, selection)?;
        let none = |reason: &str, model_name| Target {
            synthesis: Synthesis::None,
            reason: Some(reason.to_owned()),
            model_name,
            context_tokens: 0,
        };
        if research.results_only {
            return Ok((none("You asked for results only.", None), None));
        }
        if let Some((saved, destination)) = remote {
            let name = format!("{} · {}", saved.settings.name, saved.settings.model);
            if !research.share_with_provider {
                return Ok((
                    none(
                        &format!(
                            "Sending web excerpts to {} wasn't authorized for this request.",
                            saved.settings.name
                        ),
                        None,
                    ),
                    None,
                ));
            }
            if thinking {
                return Err(InferenceError::new(
                    ErrorCode::UnsupportedCapability,
                    "Thinking mode is available only with a local model. Send without thinking.",
                ));
            }
            let target = Target {
                synthesis: Synthesis::Remote,
                reason: None,
                model_name: Some(name),
                context_tokens: saved.settings.context_tokens,
            };
            return Ok((target, Some((saved, destination))));
        }
        let status = self.adapter.status()?;
        match (model_id, status.phase, status.model) {
            (Some(_), Phase::Ready, Some(model)) => {
                if thinking && !model.supports_thinking {
                    return Err(InferenceError::new(
                        ErrorCode::InvalidConfig,
                        "This model does not support thinking mode. Send without thinking.",
                    ));
                }
                Ok((
                    Target {
                        synthesis: Synthesis::Local,
                        reason: None,
                        model_name: Some(model.file_name.clone()),
                        context_tokens: model.config.context_length,
                    },
                    None,
                ))
            }
            _ => Ok((
                none(
                    "No model is loaded. Load a model to get an answer written from these sources.",
                    None,
                ),
                None,
            )),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn run_research(
        &self,
        id: ConversationId,
        action: TurnAction,
        question: String,
        research: ResearchOptions,
        options: ChatOptions,
        events: ChatEvents,
        cancellation: Cancellation,
        model_id: Option<ModelId>,
    ) -> Result<Message> {
        if options.knowledge.is_some() {
            return Err(InferenceError::new(
                ErrorCode::InvalidConfig,
                "Web search can't be combined with selected documents yet. Clear the document selection or turn off Search.",
            ));
        }
        if options
            .max_tokens
            .is_some_and(|value| !(1..=32_768).contains(&value))
        {
            return Err(InferenceError::new(
                ErrorCode::InvalidConfig,
                "Use 1–32768 output tokens.",
            ));
        }
        if self.policy.is_local_only() {
            return Err(local_only_error());
        }
        let settings = database(&self.database)?.web_research_settings()?;
        // Validate everything the user authorized before anything is saved.
        let (query, page_url, provider) = match research.mode {
            ResearchMode::Search => {
                let provider = settings.provider.ok_or_else(|| {
                    InferenceError::new(
                        ErrorCode::InvalidConfig,
                        "Choose a search provider in Settings → Web search, or read a specific webpage.",
                    )
                })?;
                if research.provider != Some(provider) {
                    return Err(InferenceError::new(
                        ErrorCode::InvalidConfig,
                        "The search provider changed. Review the search and send again.",
                    ));
                }
                let query = SearchQuery::new(research.query.as_deref().unwrap_or(&question))?;
                (Some(query), None, Some(provider))
            }
            ResearchMode::Page => {
                let url = research.url.as_deref().ok_or_else(|| {
                    InferenceError::new(ErrorCode::InvalidConfig, "Enter the webpage to read.")
                })?;
                let url = PublicUrl::parse(url).map_err(|error| {
                    InferenceError::new(ErrorCode::InvalidConfig, error.to_string())
                })?;
                (None, Some(url), None)
            }
        };
        let (target, remote_target) =
            self.research_target(&research, model_id, options.enable_thinking)?;

        let record = ResearchRecord {
            version: 1,
            mode: research.mode,
            provider,
            synthesis: target.synthesis,
            discovered: 0,
            retrieved: 0,
            unavailable: 0,
            sources: Vec::new(),
            transient_results: research.mode == ResearchMode::Search,
        };
        // The user turn and its pending reply are saved together. Research
        // answers are contextual, so they are never replayed as history.
        let store = Arc::clone(&self.database);
        let model_name = target.model_name.clone();
        let initial = AttemptContext {
            research: Some(record.clone()),
            ..Default::default()
        };
        let turn = self
            .blocking(move || {
                database(&store)?.begin_chat_with_context(
                    id,
                    action,
                    model_name.as_deref(),
                    initial,
                )
            })
            .await
            .ok_or_else(|| {
                InferenceError::new(ErrorCode::WorkerStopped, "Chat preparation stopped.")
            })??;
        let message_id = turn.assistant.id;
        deliver(
            &events,
            ChatEvent::Started {
                user: turn.user.clone(),
                assistant: Box::new(turn.assistant.clone()),
            },
        );

        let mut external: Vec<EgressRecord> = Vec::new();
        let gathered = self
            .gather(
                &research,
                query.as_ref(),
                page_url,
                provider,
                &question,
                target.context_tokens,
                &events,
                message_id,
                cancellation.clone(),
                &mut external,
            )
            .await;
        let (candidates, mut details, counts) = match gathered {
            Ok(gathered) => gathered,
            Err(error) => {
                return self.fail_research(turn, external, cancellation, error);
            }
        };
        let saved_sources = candidates
            .iter()
            .map(|candidate| {
                let save = match (research.mode, provider) {
                    (ResearchMode::Page, _) => true,
                    (_, Some(provider)) => settings.may_save(provider),
                    (_, None) => false,
                };
                WebSourceRecord {
                    evidence: candidate.evidence,
                    saved: save,
                    url: save.then(|| candidate.url.without_query()),
                    title: save.then(|| candidate.title.clone()),
                    host: save.then(|| candidate.url.host().to_owned()),
                    query_removed: save && candidate.url.has_query(),
                }
            })
            .collect::<Vec<_>>();
        let record = ResearchRecord {
            discovered: counts.0,
            retrieved: counts.1,
            unavailable: counts.2,
            transient_results: research.mode == ResearchMode::Search
                || saved_sources.iter().any(|source| !source.saved),
            sources: saved_sources,
            ..record
        };
        details.query = query.as_ref().map(|query| query.as_str().to_owned());
        details.sources = candidates
            .iter()
            .map(|candidate| TransientSource {
                title: candidate.title.clone(),
                url: candidate.url.as_str().to_owned(),
                host: candidate.url.host().to_owned(),
                published: candidate.published.clone(),
                excerpt: candidate.excerpt.clone(),
            })
            .collect();
        self.research_results.put(message_id, details);
        let mut context = turn
            .assistant
            .context
            .as_deref()
            .cloned()
            .unwrap_or_default();
        context.research = Some(record.clone());
        let save = context.clone();
        let store = Arc::clone(&self.database);
        if let Err(error) = self
            .blocking(move || database(&store)?.save_attempt_context(message_id, &save))
            .await
            .unwrap_or_else(|| {
                Err(InferenceError::new(
                    ErrorCode::WorkerStopped,
                    "Chat stopped.",
                ))
            })
        {
            return self.fail_research(turn, external, cancellation, error);
        }
        let mut turn = turn;
        turn.assistant.context = Some(Box::new(context));
        if let Err(error) = check_chat_cancellation(&cancellation) {
            return self.fail_research(turn, external, cancellation, error);
        }

        if candidates.is_empty() || target.synthesis == Synthesis::None {
            let text = if candidates.is_empty() {
                no_evidence_answer(&EvidenceTask::Web)
            } else {
                let found = match provider {
                    Some(provider) => format!(
                        "{} found {} result{}",
                        provider.label(),
                        record.discovered,
                        if record.discovered == 1 { "" } else { "s" }
                    ),
                    None => "The webpage was read".to_owned(),
                };
                format!(
                    "{found}. {} The sources are listed below.",
                    target.reason.as_deref().unwrap_or_default()
                )
            };
            self.research_event(&events, message_id, ResearchStage::Done);
            return self.finish_research_reply(turn, &events, text, external, cancellation);
        }

        self.research_event(&events, message_id, ResearchStage::Answering);
        let chat_context = ChatContext {
            evidence: Some(candidates.iter().map(Candidate::evidence_text).collect()),
            memories: Vec::new(),
            task: EvidenceTask::Web,
        };
        let mut sources = context_candidates(&turn, false);
        sources.required.push(ContextSource {
            kind: SourceKind::SystemPrompt,
            record: None,
            labels: Labels::from_source(SYSTEM_TEXT),
        });
        // Evidence answers see no earlier turns, memories or documents.
        turn.history.turns.clear();
        sources.withheld.append(&mut sources.history);
        sources.knowledge = candidates
            .iter()
            .map(|candidate| ContextSource {
                kind: SourceKind::Web,
                record: None,
                labels: candidate.labels.clone(),
            })
            .collect();
        let finished_events = Arc::clone(&events);
        let result = match (target.synthesis, remote_target) {
            (Synthesis::Remote, Some(target)) => {
                self.stream_remote(
                    remote::RemoteTurn {
                        turn,
                        context: Some(chat_context),
                        candidates: sources,
                        coverage: None,
                        sources: Vec::new(),
                        max_tokens: options.max_tokens,
                        cancellation,
                        images: Vec::new(),
                        external,
                        web: true,
                    },
                    target,
                    events,
                )
                .await
            }
            _ => {
                let model_id = model_id.ok_or_else(|| {
                    InferenceError::new(ErrorCode::NoModel, "Load a GGUF model first.")
                })?;
                let mut settings = database(&self.database)?
                    .model(model_id)?
                    .reference
                    .settings;
                settings.enable_thinking = options.enable_thinking;
                if let Some(max_tokens) = options.max_tokens {
                    settings.max_tokens = max_tokens;
                }
                settings.validate()?;
                let mut request = settings.request(
                    std::mem::take(&mut turn.prompt),
                    std::mem::take(&mut turn.history),
                );
                request.chat = Some(agent_core::inference::ChatGeneration {
                    adaptive: options.max_tokens.is_none(),
                    replay: None,
                });
                request.context = Some(chat_context);
                self.stream_chat(
                    turn,
                    request,
                    events,
                    StreamContext {
                        previous: None,
                        cancellation,
                        sources: Vec::new(),
                        coverage: None,
                        candidates: sources,
                        external,
                    },
                )
                .await
            }
        };
        self.research_event(&finished_events, message_id, ResearchStage::Done);
        result
    }

    /// Search, retrieve, extract and rank. Returns candidates in slot order,
    /// transient details and the discovered, retrieved and unavailable counts.
    #[allow(clippy::too_many_arguments)]
    async fn gather(
        &self,
        research: &ResearchOptions,
        query: Option<&SearchQuery>,
        page_url: Option<PublicUrl>,
        provider: Option<SearchProviderKind>,
        question: &str,
        context_tokens: u32,
        events: &ChatEvents,
        message_id: i64,
        cancellation: Cancellation,
        external: &mut Vec<EgressRecord>,
    ) -> Result<(Vec<Candidate>, Transient, (u32, u32, u32))> {
        let pages = if research.mode == ResearchMode::Page {
            1
        } else {
            MAX_PAGES
        };
        // One search plus each page with its redirects: the disclosed bound.
        let consent = ResearchConsent::new(
            &self.policy,
            cancellation.clone(),
            1 + pages * (1 + MAX_REDIRECTS),
            CONSENT_LIFETIME,
        )
        .map_err(|failure| match failure {
            Failure::Denied(_) => local_only_error(),
            other => crate::providers::failure_error(&other),
        })?;
        let mut details = Transient::default();
        type Targets = Vec<(PublicUrl, Labels, Option<usize>)>;
        let (results, targets): (Vec<SearchResult>, Targets) = match (query, provider, page_url) {
            (Some(query), Some(provider), _) => {
                self.research_event(events, message_id, ResearchStage::Searching);
                let results = self
                    .boundary
                    .search(&consent, provider, query, external)
                    .await?;
                // Prefer distinct sites, in provider order.
                let mut chosen: Vec<usize> = Vec::new();
                for (index, result) in results.iter().enumerate() {
                    let host = PublicUrl::parse(&result.url)
                        .ok()
                        .map(|url| url.host().to_owned());
                    if chosen.len() < pages
                        && !chosen.iter().any(|known| {
                            PublicUrl::parse(&results[*known].url)
                                .ok()
                                .map(|url| url.host().to_owned())
                                == host
                        })
                    {
                        chosen.push(index);
                    }
                }
                for index in 0..results.len() {
                    if chosen.len() >= pages {
                        break;
                    }
                    if !chosen.contains(&index) {
                        chosen.push(index);
                    }
                }
                let targets = chosen
                    .iter()
                    .filter_map(|index| {
                        PublicUrl::parse(&results[*index].url)
                            .ok()
                            .map(|url| (url, Labels::from_source(PUBLIC_WEB), Some(*index)))
                    })
                    .collect();
                (results, targets)
            }
            (_, _, Some(url)) => {
                let mut labels = Labels::from_source(USER_INPUT);
                labels.insert(PUBLIC_WEB);
                (Vec::new(), vec![(url, labels, None)])
            }
            _ => {
                return Err(InferenceError::new(
                    ErrorCode::InvalidConfig,
                    "Nothing to research.",
                ));
            }
        };
        check_chat_cancellation(&cancellation)?;
        if !targets.is_empty() {
            self.research_event(events, message_id, ResearchStage::Reading);
        }
        let records = Mutex::new(Vec::new());
        let fetches = targets.iter().map(|(url, labels, _)| {
            let records = &records;
            let consent = &consent;
            async move {
                self.boundary
                    .fetch_page(consent, url.clone(), labels, |record| {
                        if let Ok(mut records) = records.lock() {
                            records.push(record);
                        }
                    })
                    .await
            }
        });
        let fetched = futures_util::future::join_all(fetches).await;
        external.extend(records.into_inner().unwrap_or_default());
        drop(consent);
        check_chat_cancellation(&cancellation)?;
        if self.policy.is_local_only() {
            return Err(local_only_error());
        }

        self.research_event(events, message_id, ResearchStage::Ranking);
        let budget = if context_tokens == 0 {
            MAX_EVIDENCE_CHARS
        } else {
            (context_tokens as usize * 3 / (2 * MAX_WEB_SOURCES)).clamp(300, MAX_EVIDENCE_CHARS)
        };
        let terms = rank::terms(question);
        // Parsing untrusted HTML is bounded CPU work; keep it off async tasks.
        type Extracted = std::result::Result<(PublicUrl, ExtractedPage, bool), PageError>;
        let extracted: Vec<(usize, Extracted)> = self
            .blocking(move || {
                fetched
                    .into_iter()
                    .enumerate()
                    .map(|(position, outcome)| {
                        (
                            position,
                            outcome.map(|page| {
                                let parsed = extract::page(&page.body, &page.content_type);
                                (page.url, parsed, page.truncated)
                            }),
                        )
                    })
                    .collect()
            })
            .await
            .ok_or_else(|| InferenceError::new(ErrorCode::WorkerStopped, "Research stopped."))?;
        let mut candidates = Vec::new();
        let mut retrieved = 0;
        let mut unavailable = 0;
        let mut attempted = Vec::new();
        for (position, outcome) in extracted {
            let (url, labels, result_index) = &targets[position];
            if let Some(index) = result_index {
                attempted.push(*index);
            }
            let result = result_index.map(|index| &results[index]);
            let failure = match outcome {
                Ok((final_url, page, truncated)) => {
                    if page.needs_scripts {
                        Some(format!(
                            "{} needs JavaScript, which isn't run.",
                            final_url.host()
                        ))
                    } else if page.blocks.is_empty() {
                        Some(format!("{} has no readable text.", final_url.host()))
                    } else {
                        retrieved += 1;
                        let selection = rank::select(&page, &terms, budget);
                        candidates.push(Candidate {
                            evidence: EvidenceKind::Page,
                            title: page
                                .title
                                .clone()
                                .or(result.map(|result| result.title.clone()))
                                .unwrap_or_else(|| final_url.host().to_owned()),
                            published: page
                                .published
                                .clone()
                                .or(result.and_then(|result| result.published.clone())),
                            url: final_url,
                            excerpt: selection.text,
                            labels: labels.clone(),
                            coverage: selection.coverage,
                            rank: position,
                            partial: truncated || page.truncated,
                        });
                        None
                    }
                }
                Err(error) => Some(page_limitation(url.host(), &error)),
            };
            if let Some(limitation) = failure {
                unavailable += 1;
                details.limitations.push(limitation);
                // A snippet stands in only when clearly marked as one.
                if let Some(result) = result.filter(|result| !result.snippet.is_empty()) {
                    candidates.push(Candidate {
                        evidence: EvidenceKind::Snippet,
                        url: url.clone(),
                        title: result.title.clone(),
                        published: result.published.clone(),
                        excerpt: clean_text(&result.snippet, budget),
                        labels: labels.clone(),
                        coverage: rank::coverage(&result.snippet, &terms),
                        rank: position,
                        partial: false,
                    });
                }
            }
        }
        // Remaining results can fill free slots as clearly marked snippets.
        for (index, result) in results.iter().enumerate() {
            if candidates.len() >= MAX_WEB_SOURCES {
                break;
            }
            if attempted.contains(&index) || result.snippet.is_empty() {
                continue;
            }
            if let Ok(url) = PublicUrl::parse(&result.url) {
                candidates.push(Candidate {
                    evidence: EvidenceKind::Snippet,
                    url,
                    title: result.title.clone(),
                    published: result.published.clone(),
                    excerpt: clean_text(&result.snippet, budget),
                    labels: Labels::from_source(PUBLIC_WEB),
                    coverage: rank::coverage(&result.snippet, &terms),
                    rank: MAX_PAGES + index,
                    partial: false,
                });
            }
        }
        // Read pages first, then by relevance, then by provider order.
        candidates.sort_by(|a, b| {
            (a.evidence != EvidenceKind::Page)
                .cmp(&(b.evidence != EvidenceKind::Page))
                .then(b.coverage.total_cmp(&a.coverage))
                .then(a.rank.cmp(&b.rank))
        });
        candidates.truncate(MAX_WEB_SOURCES);
        details.discovered = results
            .iter()
            .enumerate()
            .filter(|(_, result)| {
                !candidates
                    .iter()
                    .any(|candidate| candidate.url.as_str() == result.url)
            })
            .map(|(index, result)| DiscoveredView {
                host: PublicUrl::parse(&result.url)
                    .map(|url| url.host().to_owned())
                    .unwrap_or_default(),
                title: result.title.clone(),
                url: result.url.clone(),
                snippet: result.snippet.clone(),
                published: result.published.clone(),
                attempted: attempted.contains(&index),
            })
            .collect();
        Ok((
            candidates,
            details,
            (results.len() as u32, retrieved, unavailable),
        ))
    }

    /// Save and stream a fixed research reply without asking a model.
    fn finish_research_reply(
        &self,
        turn: agent_core::chat::PendingTurn,
        events: &ChatEvents,
        text: String,
        external: Vec<EgressRecord>,
        cancellation: Cancellation,
    ) -> Result<Message> {
        let mut candidates = context_candidates(&turn, false);
        candidates.required.truncate(1);
        candidates.withheld.append(&mut candidates.history);
        let mut session = ChatSession::new(candidates, cancellation, SYSTEM_TEXT);
        for record in external {
            session.record_external(record);
        }
        let mut assistant = turn.assistant;
        let id = assistant.id;
        if let Err(error) = session.authorize(&self.policy, None) {
            return self.finish_failed(id, session, error);
        }
        session.fitted(Default::default(), 0)?;
        deliver(
            events,
            ChatEvent::Delta {
                id,
                text: text.clone(),
            },
        );
        let mut db = database(&self.database)?;
        let receipt = session.receipt(SessionState::Completed, None);
        db.finish_intelligence_chat(id, &text, GenerationState::Complete, None, &receipt)?;
        assistant.labels = receipt.output_labels;
        assistant.content = text;
        assistant.state = GenerationState::Complete;
        assistant.context = db.chat_attempt_context(id)?.map(Box::new);
        Ok(assistant)
    }

    fn fail_research(
        &self,
        turn: agent_core::chat::PendingTurn,
        external: Vec<EgressRecord>,
        cancellation: Cancellation,
        error: InferenceError,
    ) -> Result<Message> {
        let mut candidates = context_candidates(&turn, false);
        candidates.withheld.append(&mut candidates.history);
        let mut session = ChatSession::new(candidates, cancellation, SYSTEM_TEXT);
        for record in external {
            session.record_external(record);
        }
        self.finish_failed(turn.assistant.id, session, error)
    }

    fn finish_failed(
        &self,
        id: i64,
        session: ChatSession,
        error: InferenceError,
    ) -> Result<Message> {
        let (state, session_state) = if error.code == ErrorCode::Cancelled {
            (GenerationState::Cancelled, SessionState::Cancelled)
        } else {
            (GenerationState::Failed, SessionState::Failed)
        };
        let receipt = session.receipt(session_state, None);
        database(&self.database)?.finish_intelligence_chat(id, "", state, None, &receipt)?;
        Err(error)
    }
}
