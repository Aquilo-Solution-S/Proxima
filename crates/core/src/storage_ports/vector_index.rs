//! A store's chunk vectors behind a port (#519).
//!
//! The store keeps each entity's head, its current embedding version per
//! space, beside its rows; the index keeps the chunk vectors. The index
//! narrows candidates and the store authorizes their use: a
//! [`VectorCandidate`] is read by id under the store's owner rules before its
//! id or score is shown. A wrong or stale entry costs results, never a row.
//!
//! Without a host index the store keeps its vectors in its own database,
//! inside the transaction that changes their rows. A host index is called by
//! the store in this order: the drain writes the head and [`VectorIndex::put`]
//! before its claim commits, so a job completes only once the index has the
//! vectors. Every later head change (forget, erase, transfer, space purge,
//! the orphan sweep) leaves durable index debt in that change's
//! transaction, which maintenance applies after commit.

use std::collections::BTreeMap;
use std::sync::Mutex;

use uuid::Uuid;

use crate::{EmbeddableEntityRef, EmbeddingSpace, MemoryId, OwnerId, SpaceVector, StorageError};

/// An index hit awaiting the store's owner-RLS read by id.
///
/// Neither the id nor its score may be exposed until that read admits it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VectorCandidate {
    pub memory_id: MemoryId,
    /// Best-chunk cosine, bounded to `0..=1`.
    pub similarity_score: f32,
}

/// One entity's vectors in one space: what the index holds one version of.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VectorKey {
    pub entity_id: Uuid,
    pub space: EmbeddingSpace,
}

impl VectorKey {
    /// The order [`VectorIndex::entries`] pages in: entity id, model id,
    /// width.
    #[must_use]
    pub fn order(&self) -> (Uuid, &str, usize) {
        (
            self.entity_id,
            self.space.model_id(),
            self.space.dim().width(),
        )
    }
}

/// What the index holds for one key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VectorEntry {
    pub key: VectorKey,
    pub owner: OwnerId,
    pub version: i32,
}

/// The store's vector index when its vectors live outside its database.
///
/// Every method is idempotent, so the store may repeat any call after a
/// failure: a key the index does not hold is no error.
#[async_trait::async_trait]
pub trait VectorIndex: Send + Sync {
    /// Make `vectors` the chunks of `entity` in their space, for `owner` at
    /// `version`, replacing every earlier version. `vectors` is nonempty,
    /// in text order, and shares one space.
    async fn put(
        &self,
        entity: EmbeddableEntityRef,
        owner: OwnerId,
        version: i32,
        vectors: &[SpaceVector],
    ) -> Result<(), StorageError>;

    /// Remove every chunk of each key.
    async fn delete(&self, keys: &[VectorKey]) -> Result<(), StorageError>;

    /// Record `owner` as the owner of each key.
    async fn set_owner(&self, keys: &[VectorKey], owner: OwnerId) -> Result<(), StorageError>;

    /// Up to `window` distinct memories in `query`'s space, narrowed to
    /// `owners`. Rank by each memory's best chunk, descending by score,
    /// then by memory id descending. A goal is never a candidate. Empty
    /// owners or a zero window produce no candidates.
    async fn nearest(
        &self,
        owners: &[OwnerId],
        query: &SpaceVector,
        window: u32,
    ) -> Result<Vec<VectorCandidate>, StorageError>;

    /// Up to `limit` entries after `after`, in [`VectorKey::order`]: what
    /// the orphan sweep compares with the store's heads.
    async fn entries(
        &self,
        after: Option<&VectorKey>,
        limit: u32,
    ) -> Result<Vec<VectorEntry>, StorageError>;
}

/// A [`VectorIndex`] in process memory: the reference implementation, and
/// what a test reaches through the network form.
#[derive(Debug, Default)]
pub struct InMemoryVectorIndex {
    entries: Mutex<BTreeMap<(Uuid, String, usize), Stored>>,
}

#[derive(Debug, Clone)]
struct Stored {
    memory: bool,
    space: EmbeddingSpace,
    owner: OwnerId,
    version: i32,
    chunks: Vec<Vec<f32>>,
}

impl InMemoryVectorIndex {
    fn stored(&self) -> std::sync::MutexGuard<'_, BTreeMap<(Uuid, String, usize), Stored>> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

fn map_key(key: &VectorKey) -> (Uuid, String, usize) {
    let (entity, model, width) = key.order();
    (entity, model.to_owned(), width)
}

/// Cosine similarity in `0..=1`; a zero vector scores zero, as the
/// database's `NaN` distance does.
fn similarity(a: &[f32], b: &[f32]) -> f32 {
    let (mut dot, mut aa, mut bb) = (0.0_f64, 0.0_f64, 0.0_f64);
    for (x, y) in a.iter().zip(b) {
        dot += f64::from(*x) * f64::from(*y);
        aa += f64::from(*x) * f64::from(*x);
        bb += f64::from(*y) * f64::from(*y);
    }
    let cosine = dot / (aa.sqrt() * bb.sqrt());
    if cosine.is_nan() {
        return 0.0;
    }
    #[allow(clippy::cast_possible_truncation)]
    let score = cosine.clamp(0.0, 1.0) as f32;
    score
}

#[async_trait::async_trait]
impl VectorIndex for InMemoryVectorIndex {
    async fn put(
        &self,
        entity: EmbeddableEntityRef,
        owner: OwnerId,
        version: i32,
        vectors: &[SpaceVector],
    ) -> Result<(), StorageError> {
        let Some(first) = vectors.first() else {
            return Err(StorageError::ConstraintViolation(
                "embedding must contain at least one chunk".into(),
            ));
        };
        let space = first.space().clone();
        if vectors.iter().any(|vector| vector.space() != &space) {
            return Err(StorageError::ConstraintViolation(
                "embedding chunks must use one space".into(),
            ));
        }
        let key = VectorKey {
            entity_id: entity.entity_id(),
            space: space.clone(),
        };
        self.stored().insert(
            map_key(&key),
            Stored {
                memory: matches!(entity, EmbeddableEntityRef::Memory { .. }),
                space,
                owner,
                version,
                chunks: vectors.iter().map(|v| v.values().to_vec()).collect(),
            },
        );
        Ok(())
    }

    async fn delete(&self, keys: &[VectorKey]) -> Result<(), StorageError> {
        let mut entries = self.stored();
        for key in keys {
            entries.remove(&map_key(key));
        }
        Ok(())
    }

    async fn set_owner(&self, keys: &[VectorKey], owner: OwnerId) -> Result<(), StorageError> {
        let mut entries = self.stored();
        for key in keys {
            if let Some(stored) = entries.get_mut(&map_key(key)) {
                stored.owner = owner;
            }
        }
        Ok(())
    }

    async fn nearest(
        &self,
        owners: &[OwnerId],
        query: &SpaceVector,
        window: u32,
    ) -> Result<Vec<VectorCandidate>, StorageError> {
        let mut found: Vec<VectorCandidate> = self
            .stored()
            .iter()
            .filter(|(_, stored)| {
                stored.memory && &stored.space == query.space() && owners.contains(&stored.owner)
            })
            .map(|((entity, _, _), stored)| VectorCandidate {
                memory_id: MemoryId::new(*entity),
                similarity_score: stored
                    .chunks
                    .iter()
                    .map(|chunk| similarity(chunk, query.values()))
                    .fold(0.0, f32::max),
            })
            .collect();
        found.sort_by(|a, b| {
            b.similarity_score
                .total_cmp(&a.similarity_score)
                .then(b.memory_id.cmp(&a.memory_id))
        });
        found.truncate(usize::try_from(window).unwrap_or(usize::MAX));
        Ok(found)
    }

    async fn entries(
        &self,
        after: Option<&VectorKey>,
        limit: u32,
    ) -> Result<Vec<VectorEntry>, StorageError> {
        let start = after.map(map_key);
        Ok(self
            .stored()
            .iter()
            .filter(|(key, _)| start.as_ref().is_none_or(|start| *key > start))
            .take(usize::try_from(limit).unwrap_or(usize::MAX))
            .map(|((entity, _, _), stored)| VectorEntry {
                key: VectorKey {
                    entity_id: *entity,
                    space: stored.space.clone(),
                },
                owner: stored.owner,
                version: stored.version,
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EmbeddingDim, EntityKind, GoalId, UserId};

    fn vector(space: &EmbeddingSpace, head: [f32; 2]) -> SpaceVector {
        let mut values = vec![0.0; space.dim().width()];
        values[..2].copy_from_slice(&head);
        SpaceVector::new(space.clone(), values).expect("space width")
    }

    fn memory(id: u128) -> EmbeddableEntityRef {
        EmbeddableEntityRef::Memory {
            kind: EntityKind::Fact,
            memory_id: MemoryId::new(Uuid::from_u128(id)),
        }
    }

    /// The reference index ranks by best chunk, narrows to owners and the
    /// query's space, skips goals, and replaces a version whole.
    #[tokio::test]
    async fn the_in_memory_index_ranks_best_chunks_of_readable_memories() {
        let index = InMemoryVectorIndex::default();
        let space = EmbeddingSpace::new("m", EmbeddingDim::D384);
        let other_space = EmbeddingSpace::new("m", EmbeddingDim::D768);
        let owner = OwnerId::from(UserId::new(Uuid::from_u128(10)));
        let stranger = OwnerId::from(UserId::new(Uuid::from_u128(11)));
        let query = vector(&space, [1.0, 0.0]);
        let put = |entity, owner, vectors: Vec<SpaceVector>| {
            let index = &index;
            async move { index.put(entity, owner, 1, &vectors).await.unwrap() }
        };
        put(
            memory(1),
            owner,
            vec![vector(&space, [0.0, 1.0]), vector(&space, [1.0, 0.0])],
        )
        .await;
        put(memory(2), owner, vec![vector(&space, [1.0, 1.0])]).await;
        put(memory(3), stranger, vec![vector(&space, [1.0, 0.0])]).await;
        put(memory(4), owner, vec![vector(&other_space, [1.0, 0.0])]).await;
        put(
            EmbeddableEntityRef::Goal(GoalId::new(Uuid::from_u128(5))),
            owner,
            vec![vector(&space, [1.0, 0.0])],
        )
        .await;

        let ranked = index.nearest(&[owner], &query, 10).await.unwrap();
        let ids: Vec<u128> = ranked
            .iter()
            .map(|c| c.memory_id.into_inner().as_u128())
            .collect();
        assert_eq!(ids, [1, 2]);
        assert!((ranked[0].similarity_score - 1.0).abs() < 1e-6);
        assert!(index.nearest(&[], &query, 10).await.unwrap().is_empty());
        assert_eq!(index.nearest(&[owner], &query, 1).await.unwrap().len(), 1);

        put(memory(1), owner, vec![vector(&space, [0.0, 1.0])]).await;
        let ranked = index.nearest(&[owner], &query, 10).await.unwrap();
        assert_eq!(
            ranked[0].memory_id.into_inner().as_u128(),
            2,
            "version 1 replaced"
        );

        let key = VectorKey {
            entity_id: Uuid::from_u128(2),
            space: space.clone(),
        };
        index
            .set_owner(std::slice::from_ref(&key), stranger)
            .await
            .unwrap();
        index
            .delete(&[VectorKey {
                entity_id: Uuid::from_u128(1),
                space,
            }])
            .await
            .unwrap();
        assert!(
            index
                .nearest(&[owner], &query, 10)
                .await
                .unwrap()
                .is_empty()
        );
        let page = index.entries(None, 2).await.unwrap();
        assert_eq!(page.len(), 2);
        let rest = index.entries(Some(&page[1].key), 10).await.unwrap();
        assert_eq!(page.len() + rest.len(), 4);
        assert_eq!(page[0].key, key);
        assert_eq!(page[0].owner, stranger);
    }
}
