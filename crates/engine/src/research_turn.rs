//! Chat research with a bounded query, relevance and evidence workflow.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use agent_core::{
    chat::{ConversationId, GenerationState, Message, ModelId, TurnAction},
    chat_context::{AttemptContext, ContextUsage, EvidenceTask, no_evidence_answer},
    classification::{Labels, MODEL_OUTPUT, PUBLIC_WEB, SYSTEM_TEXT, USER_INPUT},
    inference::{Cancellation, ErrorCode, InferenceError, Result},
    intelligence::{ContextSource, EgressRecord, SessionState, SourceKind},
    research::{
        EvidenceKind, MAX_EVIDENCE_CHARS, MAX_PAGES, MAX_REDIRECTS, MAX_WEB_SOURCES, ResearchMode,
        ResearchRecord, ResearchStage, SearchProviderKind, SearchQuery, SearchResult, Synthesis,
        WebSourceRecord, clean_text,
    },
};
use inference::Phase;
use serde_json::json;

use super::{
    ChatEvent, ChatEvents, ChatOptions, ChatRuntime, check_chat_cancellation, context_candidates,
    deliver,
};
use crate::{
    chat_session::ChatSession,
    database,
    network::page::{PublicUrl, ResearchConsent},
    policy::Session,
    research::{
        ResearchOptions, SearchAttempt, Transient, TransientSource, extract, page_limitation, rank,
    },
};

#[path = "research_model.rs"]
mod model;

type RemoteTarget = (
    storage::providers::SavedProvider,
    crate::network::Destination,
);

const SEARCH_ATTEMPTS: usize = 2;
const MAX_RANKED_RESULTS: usize = 8;
const NO_SUPPORT: &str = "I couldn’t find enough relevant evidence to answer your question.";

#[derive(Default)]
struct Gathered {
    candidates: Vec<Candidate>,
    details: Transient,
    discovered: u32,
    retrieved: u32,
    unavailable: u32,
    answer: Option<String>,
}

struct PageTarget {
    url: PublicUrl,
    result: Option<SearchResult>,
    labels: Labels,
}

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
    partial: bool,
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
    ) -> Result<(Target, Option<RemoteTarget>)> {
        let selection = database(&self.database)?.intelligence_settings()?;
        let remote = crate::providers::remote_target(&self.database, selection)?;
        let none = |reason: &str, model_name| Target {
            synthesis: Synthesis::None,
            reason: Some(reason.to_owned()),
            model_name,
            context_tokens: 0,
        };
        let search = research.mode == ResearchMode::Search;
        if research.results_only && !search {
            return Ok((none("You asked for results only.", None), None));
        }
        if let Some((saved, destination)) = remote {
            let name = format!("{} · {}", saved.settings.name, saved.settings.model);
            if search && !research.query_attachments.is_empty() {
                return Err(InferenceError::new(
                    ErrorCode::PolicyDenied,
                    "Select a local Chat model to evaluate an image-derived search.",
                ));
            }
            if !research.share_with_provider {
                if search {
                    return Err(InferenceError::new(
                        ErrorCode::PolicyDenied,
                        "Allow the selected model provider to process your research question and public sources.",
                    ));
                }
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
            let target = Target {
                synthesis: if research.results_only {
                    Synthesis::None
                } else {
                    Synthesis::Remote
                },
                reason: None,
                model_name: Some(name),
                context_tokens: saved.settings.context_tokens,
            };
            return Ok((target, Some((saved, destination))));
        }
        let status = self.adapter.status()?;
        match (model_id, status.phase, status.model) {
            (Some(_), Phase::Ready, Some(model)) => Ok((
                Target {
                    synthesis: if research.results_only {
                        Synthesis::None
                    } else {
                        Synthesis::Local
                    },
                    reason: None,
                    model_name: Some(model.file_name.clone()),
                    context_tokens: model.config.context_length,
                },
                None,
            )),
            _ if search => Err(InferenceError::new(
                ErrorCode::NoModel,
                "Load a Chat model to plan and check web searches.",
            )),
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
        if !research.query_attachments.is_empty() && research.mode != ResearchMode::Search {
            return Err(InferenceError::new(
                ErrorCode::InvalidConfig,
                "Review the query from the attached image before searching.",
            ));
        }
        let settings = database(&self.database)?.web_research_settings()?;
        // Validate everything the user authorized before anything is saved.
        let (reviewed, page_url, provider) = match research.mode {
            ResearchMode::Search => {
                settings.validate()?;
                let provider = settings.provider.ok_or_else(|| {
                    InferenceError::new(
                        ErrorCode::InvalidConfig,
                        "Choose a search provider in Settings → Web search.",
                    )
                })?;
                if research.provider != Some(provider) {
                    return Err(InferenceError::new(
                        ErrorCode::InvalidConfig,
                        "The search provider changed. Review the search and send again.",
                    ));
                }
                let focus = research
                    .query
                    .as_deref()
                    .map(SearchQuery::new)
                    .transpose()?;
                let reviewed = if research.query_attachments.is_empty() {
                    if research.reviewed_query.is_some() {
                        return Err(InferenceError::new(
                            ErrorCode::InvalidConfig,
                            "A reviewed visual query requires its image provenance.",
                        ));
                    }
                    None
                } else {
                    let focus = focus
                        .filter(|query| research.reviewed_query.as_deref() == Some(query.as_str()))
                        .ok_or_else(|| {
                            InferenceError::new(
                                ErrorCode::InvalidConfig,
                                "Review the exact query from the attached image before searching.",
                            )
                        })?;
                    Some(focus)
                };
                (reviewed, None, Some(provider))
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
        let (target, remote_target) = self.research_target(&research, model_id)?;
        let query_labels = self
            .attachments
            .query_labels(id, &research.query_attachments)?;

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
        let search = provider.is_some();
        let search_limit = if !search {
            0
        } else if reviewed.is_some() {
            1
        } else {
            SEARCH_ATTEMPTS
        };
        let page_limit = if !search {
            1
        } else if research.results_only {
            0
        } else {
            MAX_PAGES
        };
        let consent = ResearchConsent::new(
            cancellation.clone(),
            search_limit + page_limit * (1 + MAX_REDIRECTS),
            CONSENT_LIFETIME,
        );
        let mut gathered = Gathered::default();
        let model = model::ResearchModel {
            runtime: self,
            remote: remote_target.as_ref(),
            session: Session::new(cancellation.clone()),
            message_id,
            answer_tokens: options.max_tokens.unwrap_or(448).min(448),
        };
        let outcome = if let Some(provider) = provider {
            self.gather_search(
                &research,
                &question,
                reviewed,
                provider,
                &query_labels,
                &model,
                &consent,
                target.context_tokens,
                &events,
                &mut external,
                &mut gathered,
            )
            .await
        } else if let Some(url) = page_url {
            self.research_event(&events, message_id, ResearchStage::Reading);
            let labels = model::web_labels(&Labels::from_source(USER_INPUT));
            let read = self
                .read_pages(
                    &consent,
                    vec![PageTarget {
                        url,
                        result: None,
                        labels: labels.clone(),
                    }],
                    &question,
                    target.context_tokens,
                    &mut external,
                    &mut gathered,
                )
                .await;
            if read.is_ok()
                && target.synthesis != Synthesis::None
                && !gathered.candidates.is_empty()
            {
                self.research_event(&events, message_id, ResearchStage::Answering);
                model
                    .select_answer(
                        &question,
                        None,
                        &gathered.candidates,
                        &labels,
                        &mut external,
                    )
                    .await
                    .map(|answer| gathered.answer = answer)
            } else {
                read
            }
        } else {
            unreachable!("validated research operation");
        };
        drop(consent);
        if let Err(error) = outcome {
            self.research_results.put(message_id, gathered.details);
            return self.fail_research(turn, external, cancellation, error);
        }
        let Gathered {
            candidates,
            mut details,
            discovered,
            retrieved,
            unavailable,
            answer,
        } = gathered;
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
            discovered,
            retrieved,
            unavailable,
            synthesis: if answer.is_none() {
                Synthesis::None
            } else {
                target.synthesis
            },
            transient_results: research.mode == ResearchMode::Search
                || saved_sources.iter().any(|source| !source.saved),
            sources: saved_sources,
            ..record
        };
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

        let grounded = answer.is_some();
        let text = answer.unwrap_or_else(|| {
            if search {
                if candidates.is_empty() {
                    NO_SUPPORT.to_owned()
                } else {
                    format!("Found {} relevant search results.", candidates.len())
                }
            } else if candidates.is_empty() {
                no_evidence_answer(&EvidenceTask::Web)
            } else if let Some(reason) = &target.reason {
                format!("The webpage was read. {reason} The source is listed below.")
            } else {
                NO_SUPPORT.to_owned()
            }
        });
        let research_model = grounded
            .then(|| {
                remote_target.as_ref().map_or_else(
                    || self.adapter.offer::<agent_core::capability::Generate>(),
                    |(saved, _)| Ok(Some(crate::providers::offer(saved))),
                )
            })
            .transpose()?
            .flatten();
        let result = self.finish_research_reply(
            turn,
            &events,
            text,
            external,
            cancellation,
            if grounded { &candidates } else { &[] },
            research_model.as_ref(),
        );
        self.research_event(&events, message_id, ResearchStage::Done);
        result
    }

    #[allow(clippy::too_many_arguments)]
    async fn gather_search(
        &self,
        research: &ResearchOptions,
        question: &str,
        reviewed: Option<SearchQuery>,
        provider: SearchProviderKind,
        origin: &Labels,
        model: &model::ResearchModel<'_>,
        consent: &ResearchConsent,
        context_tokens: u32,
        events: &ChatEvents,
        external: &mut Vec<EgressRecord>,
        gathered: &mut Gathered,
    ) -> Result<()> {
        let message_id = model.message_id;
        let mut prior: Option<SearchQuery> = None;
        let mut reason: Option<&str> = None;
        let mut tried_pages = Vec::new();
        let max_attempts = if reviewed.is_some() {
            1
        } else {
            SEARCH_ATTEMPTS
        };
        self.research_event(events, message_id, ResearchStage::Understanding);
        for attempt in 0..max_attempts {
            check_chat_cancellation(model.session.cancellation())?;
            let (query, labels) = if let Some(query) = &reviewed {
                (query.clone(), origin.clone())
            } else {
                if attempt > 0 {
                    self.research_event(events, message_id, ResearchStage::Refining);
                }
                let mut labels = origin.clone();
                if reason.is_some() {
                    labels.insert(PUBLIC_WEB);
                    labels.insert(MODEL_OUTPUT);
                }
                let plan: Option<model::QueryPlan> = model
                    .complete(
                        model::PLAN,
                        json!({
                            "question": question,
                            "focus": research.query,
                            "previousQuery": prior.as_ref().map(SearchQuery::as_str),
                            "reason": reason,
                        }),
                        &labels,
                        128,
                        external,
                    )
                    .await?;
                let Some(query) = plan.and_then(|plan| {
                    model::model_query(plan, question, research.query.as_deref(), prior.as_ref())
                }) else {
                    gathered.details.limitations.push(if prior.is_none() {
                        "The model did not produce a distinct search query.".into()
                    } else {
                        "The model did not produce a substantially different retry query.".into()
                    });
                    break;
                };
                labels.insert(MODEL_OUTPUT);
                labels.insert(SYSTEM_TEXT);
                (query, labels)
            };
            prior = Some(query.clone());
            gathered.details.query = Some(query.as_str().to_owned());
            gathered.details.attempts.push(SearchAttempt {
                query: query.as_str().to_owned(),
                results: 0,
                outcome: "Search stopped or failed",
            });
            let detail = gathered.details.attempts.len() - 1;
            self.research_event(events, message_id, ResearchStage::Searching);
            let results = self
                .boundary
                .search(consent, provider, &query, &labels, external)
                .await?;
            gathered.discovered += results.len() as u32;
            gathered.details.attempts[detail].results = results.len();
            let results: Vec<_> = results
                .into_iter()
                .filter_map(|result| PublicUrl::parse(&result.url).ok().map(|url| (url, result)))
                .take(MAX_RANKED_RESULTS)
                .collect();
            self.research_event(events, message_id, ResearchStage::Checking);
            let web_labels = model::web_labels(origin);
            let shortlist = if results.is_empty() {
                Vec::new()
            } else {
                let payload: Vec<_> = results
                    .iter()
                    .enumerate()
                    .map(|(index, (url, result))| {
                        json!({
                            "id": index + 1,
                            "title": clean_text(&result.title, 130),
                            "host": url.host(),
                            "snippet": clean_text(&result.snippet, 260),
                        })
                    })
                    .collect();
                let ranked = model
                    .complete::<model::Ranking>(
                        model::RANK,
                        json!({
                            "question": question,
                            "reviewedImageQuery": reviewed.as_ref().map(SearchQuery::as_str),
                            "results": payload,
                        }),
                        &web_labels,
                        96,
                        external,
                    )
                    .await?
                    .and_then(|rank| model::ranked(rank, results.len()));
                let Some(ranked) = ranked else {
                    reason = Some("The prior results could not be evaluated reliably.");
                    gathered.details.attempts[detail].outcome = "Unable to check results";
                    continue;
                };
                ranked
            };
            if shortlist.is_empty() {
                reason = Some("The search returned no results relevant to the question.");
                gathered.details.attempts[detail].outcome = "No relevant results";
                continue;
            }
            if research.results_only {
                gathered.candidates = shortlist
                    .into_iter()
                    .take(MAX_WEB_SOURCES)
                    .map(|index| {
                        let (url, result) = &results[index];
                        Candidate {
                            evidence: EvidenceKind::Snippet,
                            url: url.clone(),
                            title: result.title.clone(),
                            published: result.published.clone(),
                            excerpt: result.snippet.clone(),
                            labels: web_labels.clone(),
                            partial: false,
                        }
                    })
                    .collect();
                gathered.details.attempts[detail].outcome = "Relevant results";
                break;
            }

            // Keep the existing page and redirect budget across both attempts.
            // Leave one page available for a retry if the first excerpts fail.
            let pages_left = MAX_PAGES - tried_pages.len();
            let limit = if attempt + 1 < max_attempts {
                pages_left - 1
            } else {
                pages_left
            };
            let targets: Vec<_> = shortlist
                .into_iter()
                .map(|index| &results[index])
                .filter(|(url, _)| !tried_pages.iter().any(|prior| prior == url.as_str()))
                .take(limit)
                .map(|(url, result)| PageTarget {
                    url: url.clone(),
                    result: Some(result.clone()),
                    labels: web_labels.clone(),
                })
                .collect();
            tried_pages.extend(targets.iter().map(|target| target.url.as_str().to_owned()));
            if !targets.is_empty() {
                self.research_event(events, message_id, ResearchStage::Reading);
                self.read_pages(
                    consent,
                    targets,
                    question,
                    context_tokens,
                    external,
                    gathered,
                )
                .await?;
            }
            if !gathered.candidates.is_empty() {
                self.research_event(events, message_id, ResearchStage::Answering);
                if let Some(answer) = model
                    .select_answer(
                        question,
                        reviewed.as_ref(),
                        &gathered.candidates,
                        &web_labels,
                        external,
                    )
                    .await?
                {
                    gathered.details.attempts[detail].outcome = "Supporting passages found";
                    gathered.answer = Some(answer);
                    break;
                }
            }
            reason = Some(
                "Relevant-looking results did not provide readable passages that answered the question.",
            );
            gathered.details.attempts[detail].outcome = "No supporting passages";
            gathered.candidates.clear();
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn read_pages(
        &self,
        consent: &ResearchConsent,
        targets: Vec<PageTarget>,
        question: &str,
        context_tokens: u32,
        external: &mut Vec<EgressRecord>,
        gathered: &mut Gathered,
    ) -> Result<()> {
        let records = Mutex::new(Vec::new());
        let fetched = futures_util::future::join_all(targets.into_iter().map(|target| {
            let records = &records;
            async move {
                // The transport receives the URL selected from a public search
                // result. Visual provenance stays on the downstream evidence.
                let public = Labels::from_source(PUBLIC_WEB);
                let labels = if target.result.is_some() {
                    &public
                } else {
                    &target.labels
                };
                let outcome = self
                    .boundary
                    .fetch_page(consent, target.url.clone(), labels, |record| {
                        if let Ok(mut records) = records.lock() {
                            records.push(record);
                        }
                    })
                    .await;
                (target, outcome)
            }
        }))
        .await;
        external.extend(records.into_inner().unwrap_or_default());
        check_chat_cancellation(consent.session().cancellation())?;

        let budget = if context_tokens == 0 {
            MAX_EVIDENCE_CHARS
        } else {
            (context_tokens as usize * 3 / (2 * MAX_WEB_SOURCES)).clamp(300, MAX_EVIDENCE_CHARS)
        };
        let terms = rank::terms(question);
        let extracted = self
            .blocking(move || {
                fetched
                    .into_iter()
                    .map(|(target, outcome)| {
                        let parsed = outcome.map(|page| {
                            let parsed = extract::page(&page.body, &page.content_type);
                            (page.url, parsed, page.truncated)
                        });
                        (target, parsed)
                    })
                    .collect::<Vec<_>>()
            })
            .await
            .ok_or_else(|| InferenceError::new(ErrorCode::WorkerStopped, "Research stopped."))?;
        check_chat_cancellation(consent.session().cancellation())?;
        for (target, outcome) in extracted {
            let limitation = match outcome {
                Ok((url, page, _)) if page.needs_scripts => {
                    format!("{} needs JavaScript, which isn't run.", url.host())
                }
                Ok((url, page, _)) if page.blocks.is_empty() => {
                    format!("{} has no readable text.", url.host())
                }
                Ok((url, page, truncated)) => {
                    let selection = rank::select(&page, &terms, budget);
                    gathered.retrieved += 1;
                    gathered.candidates.push(Candidate {
                        evidence: EvidenceKind::Page,
                        title: page
                            .title
                            .or_else(|| target.result.as_ref().map(|result| result.title.clone()))
                            .unwrap_or_else(|| url.host().to_owned()),
                        published: page
                            .published
                            .or_else(|| target.result.and_then(|result| result.published)),
                        url,
                        excerpt: selection.text,
                        labels: target.labels,
                        partial: truncated || page.truncated,
                    });
                    continue;
                }
                Err(error) => page_limitation(target.url.host(), &error),
            };
            gathered.unavailable += 1;
            gathered.details.limitations.push(limitation);
        }
        Ok(())
    }

    /// Save and publish the validated answer or the fixed research status.
    #[allow(clippy::too_many_arguments)]
    fn finish_research_reply(
        &self,
        turn: agent_core::chat::PendingTurn,
        events: &ChatEvents,
        text: String,
        external: Vec<EgressRecord>,
        cancellation: Cancellation,
        evidence: &[Candidate],
        model: Option<&agent_core::capability::Offer>,
    ) -> Result<Message> {
        let mut candidates = context_candidates(&turn, false);
        candidates.required.truncate(1);
        candidates.withheld.append(&mut candidates.history);
        candidates.knowledge = evidence
            .iter()
            .map(|candidate| ContextSource {
                kind: SourceKind::Web,
                record: None,
                labels: candidate.labels.clone(),
            })
            .collect();
        if model.is_some() {
            candidates.required.push(ContextSource {
                kind: SourceKind::SystemPrompt,
                record: None,
                labels: Labels::from_source(SYSTEM_TEXT),
            });
        }
        let output_class = if model.is_some() {
            MODEL_OUTPUT
        } else {
            SYSTEM_TEXT
        };
        let mut session = ChatSession::new(candidates, cancellation, output_class);
        for record in external {
            session.record_external(record);
        }
        let mut assistant = turn.assistant;
        let id = assistant.id;
        if let Err(error) = session.authorize(&self.policy, None) {
            return self.finish_failed(id, session, error);
        }
        if let Some(offer) = model {
            session.submitted(offer);
        }
        let usage = ContextUsage {
            sources: evidence.len(),
            ..Default::default()
        };
        let labels = session.fitted(usage, 0)?.clone();
        let mut db = database(&self.database)?;
        if !evidence.is_empty() {
            db.checkpoint_intelligence_context(
                id,
                &labels,
                assistant.context.as_deref(),
                usage,
                &[],
            )?;
        }
        let receipt = session.receipt(SessionState::Completed, None);
        db.finish_intelligence_chat(id, &text, GenerationState::Complete, None, &receipt)?;
        assistant.labels = receipt.output_labels;
        assistant.content = text;
        assistant.state = GenerationState::Complete;
        assistant.context = db.chat_attempt_context(id)?.map(Box::new);
        drop(db);
        deliver(
            events,
            ChatEvent::Delta {
                id,
                text: assistant.content.clone(),
            },
        );
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
