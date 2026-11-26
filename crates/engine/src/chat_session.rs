//! System-derived Chat session receipts and the fitted provenance snapshot.

use std::time::Instant;

use agent_core::{
    capability::{GENERATE, Locality, Offer, Retention, Usage},
    chat_context::ContextUsage,
    classification::{DataClass, Labels},
    inference::{Cancellation, ErrorCode, GenerationStats, InferenceError, Result},
    intelligence::{ContextSource, ProviderRecord, Receipt, SessionState},
};

use crate::policy::{Denial, Operation, Permission, Policy, Session};

pub(crate) struct Candidates {
    /// Prompt, system prompt and any replay prefix.
    pub required: Vec<ContextSource>,
    pub history: Vec<ContextSource>,
    pub knowledge: Vec<ContextSource>,
    pub memories: Vec<ContextSource>,
    pub withheld: Vec<ContextSource>,
}

impl Candidates {
    fn labels(&self, output_class: DataClass) -> Labels {
        let mut labels = Labels::from_source(output_class);
        for source in self
            .required
            .iter()
            .chain(&self.history)
            .chain(&self.knowledge)
            .chain(&self.memories)
        {
            labels.inherit(&source.labels);
        }
        labels
    }
}

pub(crate) struct ChatSession {
    session: Session,
    started: Instant,
    candidates: Candidates,
    decoded: Option<Decoded>,
    provider: Option<ProviderRecord>,
    authorization: Option<&'static str>,
    output_class: DataClass,
}

struct Decoded {
    used: Vec<ContextSource>,
    withheld: Vec<ContextSource>,
    labels: Labels,
}

impl ChatSession {
    pub fn new(
        candidates: Candidates,
        cancellation: Cancellation,
        output_class: DataClass,
    ) -> Self {
        Self {
            session: Session::new(cancellation),
            started: Instant::now(),
            candidates,
            decoded: None,
            provider: None,
            authorization: None,
            output_class,
        }
    }

    pub fn authorize(&mut self, policy: &Policy, provider: Option<Offer>) -> Result<()> {
        if provider.is_some_and(|offer| {
            offer.capability != GENERATE || offer.locality != Locality::OnDevice
        }) {
            return Err(policy_error(Denial::Locality));
        }
        // Chat's accepted turn has already been written as a pending record.
        // Final output persistence is independently authorized here.
        let operation = Operation::local(
            &self.session,
            GENERATE,
            "chat",
            self.candidates.labels(self.output_class),
            Retention::SavedLocally,
        );
        let retention = policy
            .authorize_local(Permission::Retain, &operation)
            .map_err(policy_error)?;
        if provider.is_some() {
            policy
                .authorize_local(Permission::Process, &operation)
                .map_err(policy_error)?;
        }
        self.authorization = Some(retention);
        Ok(())
    }

    pub fn submitted(&mut self, offer: Offer) {
        self.provider = Some(ProviderRecord {
            id: offer.provider,
            locality: offer.locality,
            retention: offer.provider_retention,
        });
    }

    pub fn fitted(&mut self, usage: ContextUsage, excluded_history: usize) -> Result<&Labels> {
        let candidates = &self.candidates;
        let incorrect = excluded_history > candidates.history.len()
            || usage.sources > candidates.knowledge.len()
            || usage.memories > candidates.memories.len()
            || self.decoded.is_some();
        if incorrect {
            return Err(InferenceError::new(
                ErrorCode::GenerationFailed,
                "The provider reported invalid context usage.",
            ));
        }
        let (skipped_history, used_history) = candidates.history.split_at(excluded_history);
        let (used_knowledge, skipped_knowledge) = candidates.knowledge.split_at(usage.sources);
        let (used_memories, skipped_memories) = candidates.memories.split_at(usage.memories);
        let used: Vec<_> = candidates
            .required
            .iter()
            .chain(used_history)
            .chain(used_knowledge)
            .chain(used_memories)
            .cloned()
            .collect();
        let labels = Labels::derived(
            used.iter().map(|source| source.labels.clone()),
            self.output_class,
        );
        let withheld = candidates
            .withheld
            .iter()
            .chain(skipped_history)
            .chain(skipped_knowledge)
            .chain(skipped_memories)
            .cloned()
            .collect();
        Ok(&self
            .decoded
            .insert(Decoded {
                used,
                withheld,
                labels,
            })
            .labels)
    }

    pub fn is_decoded(&self) -> bool {
        self.decoded.is_some()
    }

    pub fn receipt(self, state: SessionState, stats: Option<&GenerationStats>) -> Receipt {
        self.session.close();
        let (used, withheld, output_labels) = self.decoded.map_or_else(
            || {
                // A failed or interrupted prefill can have exposed a subset
                // locally. Keep conservative labels for the persisted record.
                let labels = self.candidates.labels(self.output_class);
                let Candidates {
                    required,
                    history,
                    knowledge,
                    memories,
                    withheld,
                } = self.candidates;
                (
                    Vec::new(),
                    required
                        .into_iter()
                        .chain(history)
                        .chain(knowledge)
                        .chain(memories)
                        .chain(withheld)
                        .collect(),
                    labels,
                )
            },
            |decoded| (decoded.used, decoded.withheld, decoded.labels),
        );
        let usage = stats.map_or_else(Vec::new, |stats| {
            vec![
                Usage {
                    unit: "input-token",
                    amount: stats.prompt_tokens as u64,
                },
                Usage {
                    unit: "output-token",
                    amount: stats.generated_tokens.into(),
                },
            ]
        });
        Receipt {
            version: 1,
            capability: GENERATE,
            state,
            elapsed_ms: self
                .started
                .elapsed()
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX),
            provider: self.provider,
            local_authorization: self.authorization,
            used,
            withheld,
            output_labels,
            application_retention: Retention::SavedLocally,
            external: Vec::new(),
            usage,
        }
    }
}

pub(crate) fn policy_error(denial: Denial) -> InferenceError {
    let code = if denial == Denial::Cancelled {
        ErrorCode::Cancelled
    } else {
        ErrorCode::GenerationFailed
    };
    InferenceError::new(code, "The intelligence policy denied this request.")
}
