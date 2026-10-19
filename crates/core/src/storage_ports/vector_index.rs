//! Vector storage below the store's authorization and transaction boundary.

use super::EmbeddingWriteOutcome;
use crate::{EmbeddableEntityRef, MemoryId, OwnerId, SpaceVector, StorageError};

/// An index hit awaiting the store's owner-RLS read by id.
///
/// Neither the id nor its score may be exposed until that read admits it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VectorCandidate {
    pub memory_id: MemoryId,
    /// Best-chunk cosine, bounded to `0..=1`.
    pub similarity_score: f32,
}

/// The index narrows candidates; the store authorizes their use.
///
/// The caller supplies its transaction context so a local index participates
/// in the same transaction as the store's row changes.
#[async_trait::async_trait]
pub trait VectorIndex: Send + Sync {
    type Context: Send;

    /// Add one complete version of an entity's chunks in text order.
    /// `vectors` must be nonempty and share one space. All chunks and the
    /// current-version head are written atomically in `context`.
    async fn add(
        &self,
        context: &mut Self::Context,
        owner: &OwnerId,
        entity: EmbeddableEntityRef,
        vectors: &[SpaceVector],
    ) -> Result<EmbeddingWriteOutcome, StorageError>;

    /// Delete every version and head of this owner's entity in every space.
    async fn delete(
        &self,
        context: &mut Self::Context,
        owner: &OwnerId,
        entity: EmbeddableEntityRef,
    ) -> Result<(), StorageError>;

    /// Up to `window` distinct memories in `query`'s space, narrowed to `owners`.
    /// Rank by the best chunk of each memory's current complete version,
    /// descending by score, then by memory id. Empty owners or a zero window
    /// produce no candidates.
    async fn nearest(
        &self,
        context: &mut Self::Context,
        owners: &[OwnerId],
        query: &SpaceVector,
        window: u32,
    ) -> Result<Vec<VectorCandidate>, StorageError>;
}
