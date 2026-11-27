//! The curated, hash-pinned catalog for local model downloads and imports.
//! A file is "verified" only when its size and full SHA-256 match an entry;
//! any other file stays an unverified user import.

use agent_core::capability::{AGENT_DECISION, CapabilityId, EMBED, GENERATE};
use serde::Serialize;

pub const MANIFEST_VERSION: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Artifact {
    /// Stable identity recorded with a verified import.
    pub key: &'static str,
    pub name: &'static str,
    pub file_name: &'static str,
    pub purpose: Purpose,
    pub description: &'static str,
    pub runtime: &'static str,
    pub format: &'static str,
    pub bytes: u64,
    pub sha256: &'static str,
    /// Where the exact file was published.
    pub source: &'static str,
    pub revision: &'static str,
    pub license_url: &'static str,
    pub license: &'static str,
    pub capabilities: &'static [CapabilityId],
    pub thinking: bool,
    /// Languages the publisher documents, or empty when unspecified.
    pub languages: &'static [&'static str],
    pub language_note: Option<&'static str>,
    pub trained_context: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Purpose {
    Everyday,
    Advanced,
    Knowledge,
}

/// LFM2.5-1.2B-Instruct Q4_K_M. Formerly bundled with the Debian package.
pub const LFM25: Artifact = Artifact {
    key: "lfm2.5-1.2b-instruct-q4_k_m",
    name: "LFM2.5 1.2B Instruct (Q4_K_M)",
    file_name: "LFM2.5-1.2B-Instruct-Q4_K_M.gguf",
    purpose: Purpose::Everyday,
    description: "A small, fast assistant for everyday Chat on your computer.",
    runtime: "llama.cpp (CPU)",
    format: "gguf",
    bytes: 730_895_168,
    sha256: "b1b3de114215d9507409a662a501a631095a479a419584e8a2ded6304b19b4f5",
    source: "huggingface.co/LiquidAI/LFM2.5-1.2B-Instruct-GGUF",
    revision: "8ed288026e23958ad9dfa92d53ed773a8eee7125",
    license_url: "https://huggingface.co/LiquidAI/LFM2.5-1.2B-Instruct-GGUF/blob/8ed288026e23958ad9dfa92d53ed773a8eee7125/LICENSE",
    license: "LFM Open License v1.0",
    capabilities: &[GENERATE],
    thinking: false,
    languages: &["en", "ar", "zh", "fr", "de", "ja", "ko", "es"],
    language_note: None,
    trained_context: 128_000,
};

/// Exact Unsloth quantization previously exercised by the local runtime.
pub const QWEN35: Artifact = Artifact {
    key: "qwen3.5-4b-ud-q4_k_xl",
    name: "Qwen3.5 4B (UD-Q4_K_XL)",
    file_name: "Qwen3.5-4B-UD-Q4_K_XL.gguf",
    purpose: Purpose::Advanced,
    description: "Text Chat, Thinking and supported Agent tasks. Needs more memory and CPU time.",
    runtime: "llama.cpp (CPU)",
    format: "gguf",
    bytes: 2_912_109_728,
    sha256: "b252c5610a42ca82d20fe2a12813e9d069eed89292907e26c783eeb0bc961bc7",
    source: "huggingface.co/unsloth/Qwen3.5-4B-GGUF",
    revision: "6727efff692673b7a2ebb4c6ba830ce4f5fb1309",
    license_url: "https://huggingface.co/Qwen/Qwen3.5-4B/blob/851bf6e806efd8d0a36b00ddf55e13ccb7b8cd0a/LICENSE",
    license: "Apache-2.0",
    capabilities: &[GENERATE, AGENT_DECISION],
    thinking: true,
    languages: &[],
    language_note: Some("The publisher reports support for 201 languages and dialects."),
    trained_context: 262_144,
};

/// BGE small English v1.5, F16 GGUF conversion. The pinned embedding model.
pub const BGE_SMALL_EN: Artifact = Artifact {
    key: "bge-small-en-v1.5-f16",
    name: "BGE small English v1.5 (F16)",
    file_name: "bge-small-en-v1.5-f16.gguf",
    purpose: Purpose::Knowledge,
    description: "Find related passages in English documents with semantic or hybrid search.",
    runtime: "llama.cpp (CPU)",
    format: "gguf",
    bytes: 67_308_128,
    sha256: agent_core::semantic::BGE_SHA256,
    source: "huggingface.co/CompendiumLabs/bge-small-en-v1.5-gguf",
    revision: "d32f8c040ea3b516330eeb75b72bcc2d3a780ab7",
    license_url: "https://huggingface.co/BAAI/bge-small-en-v1.5/blob/5c38ec7c405ec4b44b94cc5a9bb96e735b38267a/README.md",
    license: "MIT",
    capabilities: &[EMBED],
    thinking: false,
    languages: &["en"],
    language_note: None,
    trained_context: agent_core::semantic::CONTEXT_TOKENS as u32,
};

pub const CURATED: &[Artifact] = &[LFM25, QWEN35, BGE_SMALL_EN];

/// Entries a file of this size could match. Only these need a full digest.
pub fn candidates(bytes: u64) -> impl Iterator<Item = &'static Artifact> {
    CURATED
        .iter()
        .filter(move |artifact| artifact.bytes == bytes)
}

/// The curated entry for an exact size and lowercase SHA-256.
pub fn identify(bytes: u64, sha256: &str) -> Option<&'static Artifact> {
    candidates(bytes).find(|artifact| artifact.sha256 == sha256)
}

pub fn by_key(key: &str) -> Option<&'static Artifact> {
    CURATED.iter().find(|artifact| artifact.key == key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_entries_are_complete_and_unique() {
        for (index, artifact) in CURATED.iter().enumerate() {
            assert_eq!(artifact.sha256.len(), 64);
            assert!(
                artifact
                    .sha256
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            );
            assert_eq!(artifact.revision.len(), 40);
            assert!(
                artifact.bytes > 0
                    && !artifact.license.is_empty()
                    && !artifact.capabilities.is_empty()
            );
            assert!(artifact.file_name.ends_with(".gguf"));
            assert!(
                CURATED[index + 1..]
                    .iter()
                    .all(|other| other.key != artifact.key && other.sha256 != artifact.sha256)
            );
        }
    }

    #[test]
    fn identification_requires_size_and_digest() {
        assert_eq!(identify(LFM25.bytes, LFM25.sha256), Some(&LFM25));
        assert_eq!(identify(LFM25.bytes - 1, LFM25.sha256), None);
        assert_eq!(identify(LFM25.bytes, BGE_SMALL_EN.sha256), None);
        assert_eq!(candidates(12).count(), 0);
        assert_eq!(by_key("bge-small-en-v1.5-f16"), Some(&BGE_SMALL_EN));
    }
}
