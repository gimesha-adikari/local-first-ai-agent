//! The curated, hash-pinned manifest for optional local model imports.
//! A file is "verified" only when its size and full SHA-256 match an entry;
//! any other file stays an unverified user import. Nothing is downloaded.

use agent_core::capability::{CapabilityId, EMBED, GENERATE};
use serde::Serialize;

pub const MANIFEST_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Artifact {
    /// Stable identity recorded with a verified import.
    pub key: &'static str,
    pub name: &'static str,
    pub file_name: &'static str,
    pub format: &'static str,
    pub bytes: u64,
    pub sha256: &'static str,
    /// Where the exact file was published.
    pub source: &'static str,
    pub revision: &'static str,
    pub license: &'static str,
    pub capabilities: &'static [CapabilityId],
    /// Languages the publisher documents, or empty when unspecified.
    pub languages: &'static [&'static str],
}

/// LFM2.5-1.2B-Instruct Q4_K_M. Formerly bundled with the Debian package.
pub const LFM25: Artifact = Artifact {
    key: "lfm2.5-1.2b-instruct-q4_k_m",
    name: "LFM2.5 1.2B Instruct (Q4_K_M)",
    file_name: "LFM2.5-1.2B-Instruct-Q4_K_M.gguf",
    format: "gguf",
    bytes: 730_895_168,
    sha256: "b1b3de114215d9507409a662a501a631095a479a419584e8a2ded6304b19b4f5",
    source: "huggingface.co/LiquidAI/LFM2.5-1.2B-Instruct-GGUF",
    revision: "8ed288026e23958ad9dfa92d53ed773a8eee7125",
    license: "LFM Open License v1.0",
    capabilities: &[GENERATE],
    languages: &[],
};

/// BGE small English v1.5, F16 GGUF conversion. The pinned embedding model.
pub const BGE_SMALL_EN: Artifact = Artifact {
    key: "bge-small-en-v1.5-f16",
    name: "BGE small English v1.5 (F16)",
    file_name: "bge-small-en-v1.5-f16.gguf",
    format: "gguf",
    bytes: 67_308_128,
    sha256: agent_core::semantic::BGE_SHA256,
    source: "huggingface.co/CompendiumLabs/bge-small-en-v1.5-gguf",
    revision: "d32f8c040ea3b516330eeb75b72bcc2d3a780ab7",
    license: "MIT",
    capabilities: &[EMBED],
    languages: &["en"],
};

pub const CURATED: &[Artifact] = &[LFM25, BGE_SMALL_EN];

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
