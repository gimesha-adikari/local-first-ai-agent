//! Versioned embedding vectors and reciprocal rank fusion for local retrieval.

use crate::agent::{AgentError, AgentErrorCode, AgentResult};

pub const DIMENSIONS: usize = 384;
pub const VECTOR_BYTES: usize = DIMENSIONS * size_of::<f32>();
pub const CONTEXT_TOKENS: usize = 512;
pub const WINDOW_TOKENS: usize = CONTEXT_TOKENS - 2;
pub const WINDOW_OVERLAP: usize = 32;
pub const MAX_CHUNK_WINDOWS: usize = 8;
pub const MAX_DOCUMENT_WINDOWS: usize = 4096;
pub const MAX_LIBRARY_WINDOWS: usize = 32768;
pub const RETRIEVAL_CANDIDATES: usize = 60;
/// Minimum query–passage cosine similarity for a dense hit to count as relevant.
/// Calibrated against BGE-small-en-v1.5 with positive, paraphrased and unrelated
/// queries (see the opt-in `verified_bge_relevance_floor_*` inference test).
pub const RELEVANCE_FLOOR: f64 = 0.56;
/// A dense hit must also score within this distance of the best hit, so a
/// strongly relevant passage does not bring weakly related neighbours along.
pub const RELEVANCE_MARGIN: f64 = 0.15;
pub const QUERY_PREFIX: &str = "Represent this sentence for searching relevant passages: ";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmbeddingKey {
    pub fingerprint: [u8; 32],
    pub preprocessing: u32,
}

pub const BGE_SHA256: &str = "f0b2fef971e8366438bfd2d9aefea1b0115919389448806d290237f638bae999";
pub const BGE_KEY: EmbeddingKey = EmbeddingKey {
    fingerprint: [
        0xf0, 0xb2, 0xfe, 0xf9, 0x71, 0xe8, 0x36, 0x64, 0x38, 0xbf, 0xd2, 0xd9, 0xae, 0xfe, 0xa1,
        0xb0, 0x11, 0x59, 0x19, 0x38, 0x94, 0x48, 0x80, 0x6d, 0x29, 0x02, 0x37, 0xf6, 0x38, 0xba,
        0xe9, 0x99,
    ],
    preprocessing: 1,
};

#[derive(Debug)]
pub struct UnitVector([f32; DIMENSIONS]);

impl UnitVector {
    pub fn normalize(values: &[f32]) -> AgentResult<Self> {
        let array: &[f32; DIMENSIONS] = values.try_into().map_err(|_| incompatible())?;
        let norm = squared_norm(array).sqrt();
        if !norm.is_finite() || norm == 0.0 {
            return Err(incompatible());
        }
        Ok(Self(array.map(|x| (f64::from(x) / norm) as f32)))
    }

    pub fn from_bytes(bytes: &[u8]) -> AgentResult<Self> {
        if bytes.len() != VECTOR_BYTES {
            return Err(incompatible());
        }
        let mut values = [0.0; DIMENSIONS];
        for (value, bytes) in values.iter_mut().zip(bytes.as_chunks::<4>().0) {
            *value = f32::from_le_bytes(*bytes);
        }
        let norm = squared_norm(&values);
        if !norm.is_finite() || (norm - 1.0).abs() > 0.001 {
            return Err(incompatible());
        }
        Ok(Self(values))
    }

    pub fn to_bytes(&self) -> [u8; VECTOR_BYTES] {
        let mut bytes = [0; VECTOR_BYTES];
        for (dest, value) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(self.0) {
            dest.copy_from_slice(&value.to_le_bytes());
        }
        bytes
    }

    pub fn cosine(&self, other: &Self) -> f64 {
        let sum: f64 = self
            .0
            .iter()
            .zip(other.0)
            .map(|(&a, b)| f64::from(a) * f64::from(b))
            .sum();
        sum.clamp(-1.0, 1.0)
    }
}

fn squared_norm(values: &[f32]) -> f64 {
    values.iter().map(|&value| f64::from(value).powi(2)).sum()
}

pub fn incompatible() -> AgentError {
    AgentError::new(
        AgentErrorCode::EmbeddingIncompatible,
        "The embedding model or stored vectors are incompatible.",
    )
}

#[derive(Debug)]
pub struct EmbeddingWindow {
    /// Half-open offsets in the parent chunk's token stream, excluding CLS and SEP.
    pub start: u32,
    pub end: u32,
    pub vector: UnitVector,
}

pub type ChunkKey = (i64, u32);

/// Keep only dense candidates that pass the absolute floor and lie within the
/// relative margin of the best candidate. Input must be ranked best first.
pub fn relevant_candidates(ranked: Vec<(ChunkKey, f64)>) -> Vec<(ChunkKey, f64)> {
    let Some(&(_, best)) = ranked.first() else {
        return ranked;
    };
    let threshold = RELEVANCE_FLOOR.max(best - RELEVANCE_MARGIN);
    ranked
        .into_iter()
        .take_while(|(_, score)| *score >= threshold)
        .collect()
}

/// Keep dense candidates that pass the absolute floor. Hybrid fusion uses
/// this so a passage that also matches by keyword keeps its dense rank.
pub fn above_floor(ranked: Vec<(ChunkKey, f64)>) -> Vec<(ChunkKey, f64)> {
    ranked
        .into_iter()
        .take_while(|(_, score)| *score >= RELEVANCE_FLOOR)
        .collect()
}

pub fn reciprocal_rank_fusion(
    lexical: impl IntoIterator<Item = ChunkKey>,
    semantic: impl IntoIterator<Item = ChunkKey>,
) -> Vec<(ChunkKey, f64)> {
    use std::collections::{BTreeMap, BTreeSet};

    let mut scores = BTreeMap::<ChunkKey, f64>::new();
    for list in [
        lexical
            .into_iter()
            .take(RETRIEVAL_CANDIDATES)
            .collect::<Vec<_>>(),
        semantic.into_iter().take(RETRIEVAL_CANDIDATES).collect(),
    ] {
        let mut seen = BTreeSet::new();
        for (rank, key) in list.into_iter().enumerate() {
            if seen.insert(key) {
                *scores.entry(key).or_default() += 1.0 / (61 + rank) as f64;
            }
        }
    }
    let mut scores: Vec<_> = scores.into_iter().collect();
    scores.sort_by(|(a_id, a), (b_id, b)| b.total_cmp(a).then_with(|| a_id.cmp(b_id)));
    scores
}

#[cfg(test)]
#[test]
fn relevance_filter_applies_floor_and_margin() {
    assert!(relevant_candidates(vec![((1, 0), 0.55), ((1, 1), 0.40)]).is_empty());
    assert_eq!(
        relevant_candidates(vec![((1, 0), 0.80), ((2, 0), 0.70), ((3, 0), 0.60)]),
        vec![((1, 0), 0.80), ((2, 0), 0.70)]
    );
    assert_eq!(
        relevant_candidates(vec![((1, 0), 0.61), ((2, 0), 0.58), ((3, 0), 0.50)]).len(),
        2
    );
    assert!(relevant_candidates(Vec::new()).is_empty());
}
