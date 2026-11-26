//! Versioned capability discovery and the semantics of a single invocation.

use std::collections::BTreeSet;

use serde::Serialize;

use crate::{
    agent::{DecisionRequest, DecisionResponse},
    inference::{GenerationRequest, GenerationStats, TextDelta},
};

/// Static identifiers come from the trusted application registry. Incoming
/// strings resolve by exact equality, including the version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct CapabilityId(&'static str);

impl CapabilityId {
    pub const fn new(id: &'static str) -> Self {
        Self(id)
    }

    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

pub const GENERATE: CapabilityId = CapabilityId::new("core.generate/1");
pub const AGENT_DECISION: CapabilityId = CapabilityId::new("core.agent-decision/1");
pub const EMBED: CapabilityId = CapabilityId::new("core.embed/1");
pub const IMPORT: CapabilityId = CapabilityId::new("core.import/1");
pub const RETRIEVE: CapabilityId = CapabilityId::new("core.retrieve/1");

#[derive(Debug, Clone)]
pub struct CapabilityRegistry(BTreeSet<CapabilityId>);

impl CapabilityRegistry {
    pub fn new(ids: impl IntoIterator<Item = CapabilityId>) -> Self {
        Self(ids.into_iter().filter(|id| valid_id(id.as_str())).collect())
    }

    pub fn resolve(&self, id: &str) -> Option<CapabilityId> {
        self.0.iter().copied().find(|known| known.as_str() == id)
    }
}

pub(crate) fn valid_id(id: &str) -> bool {
    let Some((name, version)) = id.split_once('/') else {
        return false;
    };
    name.contains('.')
        && name.split('.').all(|segment| {
            segment
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_lowercase)
                && segment
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
        && !version.starts_with('0')
        && version.bytes().all(|b| b.is_ascii_digit())
        && version.parse::<u32>().is_ok()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Locality {
    OnDevice,
    PairedDevice,
    SelfHosted,
    ThirdParty,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Retention {
    None,
    Session,
    SavedLocally,
    ProviderDeclared,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Languages {
    Unspecified,
    Listed(&'static [&'static str]),
    NotApplicable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Usage {
    pub unit: &'static str,
    pub amount: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct UnitPrice {
    pub unit: &'static str,
    pub currency: &'static str,
    pub millionths_per_unit: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Resources {
    pub cpu_threads: Option<u32>,
    /// A lower bound accounted for by model weights alone.
    pub weight_bytes: Option<u64>,
    pub context: Option<Usage>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Offer {
    pub capability: CapabilityId,
    pub provider: &'static str,
    pub locality: Locality,
    pub input_modalities: &'static [&'static str],
    pub output_modalities: &'static [&'static str],
    pub input_languages: Languages,
    pub output_languages: Languages,
    pub streaming: bool,
    pub structured_output: bool,
    pub resources: Resources,
    pub usage_units: &'static [&'static str],
    pub unit_prices: &'static [UnitPrice],
    pub provider_retention: Retention,
}

/// New capabilities declare their own types without changing an enum of
/// every capability in the system.
pub trait Capability {
    const ID: CapabilityId;
    type Input;
    type Output;
    type Delta;
}

pub struct Generate;

impl Capability for Generate {
    const ID: CapabilityId = GENERATE;
    type Input = GenerationRequest;
    type Output = GenerationStats;
    type Delta = TextDelta;
}

pub struct Decide;

impl Capability for Decide {
    const ID: CapabilityId = AGENT_DECISION;
    type Input = DecisionRequest;
    type Output = DecisionResponse;
    type Delta = std::convert::Infallible;
}

pub struct Invocation<C: Capability> {
    pub input: C::Input,
    pub cancellation: crate::inference::Cancellation,
}

impl<C: Capability> Invocation<C> {
    pub fn new(input: C::Input, cancellation: crate::inference::Cancellation) -> Self {
        Self {
            input,
            cancellation,
        }
    }
}

/// An adapter owns the execution machinery and any provider-specific resume
/// format. The coordinator supplies one typed request and a stream sink.
pub trait Invoke<C: Capability> {
    type Pending;

    fn invoke(
        &self,
        request: Invocation<C>,
        emit: impl FnMut(C::Delta) -> crate::inference::Result<()> + Send + 'static,
    ) -> crate::inference::Result<Self::Pending>;
}
