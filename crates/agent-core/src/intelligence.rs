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
    SystemPrompt,
    History,
    Knowledge,
    Memory,
    AssistantPrefix,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContextSource {
    pub kind: SourceKind,
    /// Local record identifier when the context item comes from storage.
    pub record: Option<i64>,
    pub labels: Labels,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ProviderRecord {
    pub id: &'static str,
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

#[derive(Debug, Clone, Serialize)]
pub struct EgressRecord {
    pub destination: &'static str,
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
}
