//! Versioned capability discovery and the semantics of a single invocation.

use std::{borrow::Cow, collections::BTreeSet};

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
/// Model discovery and connection tests against a configured provider.
pub const DISCOVER: CapabilityId = CapabilityId::new("core.provider-discovery/1");

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

/// A configured or provider-declared price. The amount is in millionths of
/// the currency unit per million usage units, so fractional per-token prices
/// stay exact integers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UnitPrice {
    pub unit: &'static str,
    pub currency: String,
    pub micros_per_million: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Resources {
    pub cpu_threads: Option<u32>,
    /// A lower bound accounted for by model weights alone.
    pub weight_bytes: Option<u64>,
    pub context: Option<Usage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Offer {
    pub capability: CapabilityId,
    /// Registry identity for built-in adapters, or the configured provider
    /// identity for a remote endpoint.
    pub provider: Cow<'static, str>,
    pub locality: Locality,
    pub input_modalities: &'static [&'static str],
    pub output_modalities: &'static [&'static str],
    pub input_languages: Languages,
    pub output_languages: Languages,
    pub streaming: bool,
    pub structured_output: bool,
    pub resources: Resources,
    pub usage_units: &'static [&'static str],
    pub unit_prices: Vec<UnitPrice>,
    pub provider_retention: Retention,
}

pub const TEXT: &str = "text";
pub const IMAGE: &str = "image";

impl Offer {
    pub fn accepts(&self, modality: &str) -> bool {
        self.input_modalities.contains(&modality)
    }
}

/// Image media accepted by typed image input. Other formats are rejected at
/// the trusted boundary that constructs the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ImageFormat {
    Png,
    Jpeg,
    Webp,
}

impl ImageFormat {
    pub fn media_type(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Webp => "image/webp",
        }
    }

    /// Detect the format from its signature. Declared types are not trusted.
    pub fn sniff(bytes: &[u8]) -> Option<Self> {
        if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            Some(Self::Png)
        } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
            Some(Self::Jpeg)
        } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
            Some(Self::Webp)
        } else {
            None
        }
    }
}

/// Largest image accepted for one request, before encoding.
pub const MAX_IMAGE_BYTES: usize = 3 * 1024 * 1024;

/// One image for a generation request. Construction checks the signature and
/// size. Its classification travels separately in [`crate::classification::Classified`].
#[derive(Clone, PartialEq, Eq)]
pub struct ImageInput {
    format: ImageFormat,
    bytes: Vec<u8>,
}

impl std::fmt::Debug for ImageInput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ImageInput")
            .field("format", &self.format)
            .field("bytes", &self.bytes.len())
            .finish()
    }
}

impl ImageInput {
    pub fn new(bytes: Vec<u8>) -> Option<Self> {
        if bytes.is_empty() || bytes.len() > MAX_IMAGE_BYTES {
            return None;
        }
        Some(Self {
            format: ImageFormat::sniff(&bytes)?,
            bytes,
        })
    }

    pub fn format(&self) -> ImageFormat {
        self.format
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
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
