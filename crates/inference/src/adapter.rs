//! Request adapter for the on-device inference worker.

use std::sync::Arc;

use agent_core::{
    agent::DecisionResponse,
    capability::{
        AGENT_DECISION, Capability, Decide, GENERATE, Generate, Invocation, Invoke, Languages,
        Locality, Offer, Resources, Retention, Usage,
    },
    inference::{GenerationStats, Result, TextDelta},
};

use crate::{InferenceWorker, Pending};

#[derive(Clone)]
pub struct LocalInference {
    worker: Arc<InferenceWorker>,
}

impl LocalInference {
    pub fn new(worker: Arc<InferenceWorker>) -> Self {
        Self { worker }
    }

    pub fn status(&self) -> Result<crate::Status> {
        self.worker.status()
    }

    pub fn discover(&self) -> Result<Vec<Offer>> {
        let Some(model) = self.worker.status()?.model else {
            return Ok(Vec::new());
        };
        let generate = Offer {
            capability: GENERATE,
            provider: "local.llama-cpp/1",
            locality: Locality::OnDevice,
            input_modalities: &["text"],
            output_modalities: &["text"],
            input_languages: Languages::Unspecified,
            output_languages: Languages::Unspecified,
            streaming: true,
            structured_output: false,
            resources: Resources {
                cpu_threads: Some(model.config.threads),
                weight_bytes: Some(model.file_bytes),
                context: Some(Usage {
                    unit: "token",
                    amount: model.config.context_length.into(),
                }),
            },
            usage_units: &["input-token", "output-token"],
            unit_prices: &[],
            provider_retention: Retention::None,
        };
        let mut offers = vec![generate];
        if model.supports_agent {
            offers.push(Offer {
                capability: AGENT_DECISION,
                streaming: false,
                structured_output: true,
                ..generate
            });
        }
        Ok(offers)
    }

    pub fn offer<C: Capability>(&self) -> Result<Option<Offer>> {
        Ok(self
            .discover()?
            .into_iter()
            .find(|offer| offer.capability == C::ID))
    }
}

impl Invoke<Generate> for LocalInference {
    type Pending = Pending<GenerationStats>;

    fn invoke(
        &self,
        request: Invocation<Generate>,
        emit: impl FnMut(TextDelta) -> Result<()> + Send + 'static,
    ) -> Result<Self::Pending> {
        self.worker
            .generate_with_cancellation(request.input, request.cancellation, emit)
    }
}

impl Invoke<Decide> for LocalInference {
    type Pending = Pending<DecisionResponse>;

    fn invoke(
        &self,
        request: Invocation<Decide>,
        _emit: impl FnMut(std::convert::Infallible) -> Result<()> + Send + 'static,
    ) -> Result<Self::Pending> {
        self.worker.decide(request.input, request.cancellation)
    }
}
