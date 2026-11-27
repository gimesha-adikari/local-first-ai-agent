//! Metadata recorded by the intelligence runtime and network boundary.

use serde::Serialize;

use crate::{
    capability::{CapabilityId, Locality, Retention, Usage},
    classification::Labels,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Completed,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Prompt,
    Clipboard,
    ManualText,
    ScreenText,
    ScreenImage,
    SystemPrompt,
    History,
    Knowledge,
    Memory,
    AssistantPrefix,
    /// Evidence from a public webpage or search result for a research turn.
    Web,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContextSource {
    pub kind: SourceKind,
    /// Local record identifier when the context item comes from storage.
    pub record: Option<i64>,
    pub labels: Labels,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderRecord {
    pub id: std::borrow::Cow<'static, str>,
    pub locality: Locality,
    pub retention: Retention,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Transmission {
    Unsent,
    Sent,
    Unknown,
}

/// One transport attempt, recorded by the trusted boundary. It never holds
/// the payload, credentials or response content.
#[derive(Debug, Clone, Serialize)]
pub struct EgressRecord {
    pub destination: String,
    /// Canonical scheme, host and port the attempt was bound to.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub origin: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub method: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub route: String,
    /// SHA-256 of the immutable request body, as lowercase hex.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub body_sha256: String,
    #[serde(default)]
    pub body_bytes: u64,
    /// Whether a stored credential was attached at the transport edge.
    #[serde(default)]
    pub credential: bool,
    pub locality: Locality,
    pub provider_retention: Retention,
    pub transmission: Transmission,
    pub labels: Labels,
    // IDs are local to the policy instance.
    pub authorization: Vec<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Receipt {
    pub version: u32,
    pub capability: CapabilityId,
    pub state: SessionState,
    pub elapsed_ms: u64,
    pub provider: Option<ProviderRecord>,
    /// Local processing/retention rule in effect for this session.
    pub local_authorization: Option<&'static str>,
    /// Items in the fully decoded prompt, including an existing replay prefix.
    pub used: Vec<ContextSource>,
    /// Candidates excluded by routing, context fitting or an incomplete prefill.
    pub withheld: Vec<ContextSource>,
    pub output_labels: Labels,
    pub application_retention: Retention,
    pub external: Vec<EgressRecord>,
    pub usage: Vec<Usage>,
    /// Where the usage figures came from. Absent on local receipts written
    /// before 03-C, which always measured usage on the device.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_basis: Option<UsageBasis>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageBasis {
    /// Counted by the local runtime.
    Measured,
    /// Reported by the provider in its response.
    ProviderReported,
    /// No report was received; the figures are the application's estimate.
    Estimated,
    /// Nothing was transmitted, so there is no usage.
    None,
}
