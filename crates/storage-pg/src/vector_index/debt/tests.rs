use std::sync::Arc;

use proxima_core::storage_ports::{InMemoryVectorIndex, VectorIndex};
use proxima_core::{EmbeddableEntityRef, EntityKind, MemoryId, SpaceVector, UserId};
use uuid::Uuid;

use super::*;
use crate::test_fixtures::fresh_pg;

/// Index debt is read from the database, so its rows are parsed at that
/// spring: an operation or width the store does not know is refused.
#[test]
fn debt_rows_parse_only_known_operations_and_widths() {
    let id = Uuid::from_u128(1);
    let row = |dim: i16, op: &str| (id, "debt".to_owned(), dim, id, op.to_owned());
    assert!(matches!(
        parse_debt(row(384, "delete")).map(|d| d.op),
        Ok(DebtOp::Delete)
    ));
    assert!(matches!(
        parse_debt(row(384, "owner")).map(|d| d.op),
        Ok(DebtOp::Owner)
    ));
    assert!(matches!(
        parse_debt(row(384, "rename")),
        Err(StorageError::ConstraintViolation(_))
    ));
    assert!(matches!(
        parse_debt(row(385, "delete")),
        Err(StorageError::ConstraintViolation(_))
    ));
}

/// An erase pays, before it commits, the debt its own transaction left and
/// the debt an earlier forget of the same owner left for the drain; another
/// owner's debt stays for the drain.
#[tokio::test]
async fn an_erase_settles_its_owners_debt_before_commit() {
    let (pg, _guard) = fresh_pg("vector_index_settle").await;
    let pool = pg.pool_for_tests();
    let (owner, other) = (
        OwnerId::from(UserId::new(Uuid::now_v7())),
        OwnerId::from(UserId::new(Uuid::now_v7())),
    );
    let index = Arc::new(InMemoryVectorIndex::default());
    let (erased, forgotten, foreign) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    for (entity, owner) in [(erased, owner), (forgotten, owner), (foreign, other)] {
        sqlx::query(
            "INSERT INTO proxima_core.owners (owner_id, kind) VALUES ($1, 'personal')
             ON CONFLICT DO NOTHING",
        )
        .bind(owner.into_inner())
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO proxima_core.embedding_heads (entity_id, model_id, dim, embedding_version, owner_id)
             VALUES ($1, 'debt', 384, 1, $2)",
        )
        .bind(entity)
        .bind(owner.into_inner())
        .execute(pool)
        .await
        .unwrap();
        let vector = SpaceVector::new(
            EmbeddingSpace::new("debt", EmbeddingDim::D384),
            vec![1.0; 384],
        )
        .unwrap();
        let entity = EmbeddableEntityRef::Memory {
            kind: EntityKind::Fact,
            memory_id: MemoryId::new(entity),
        };
        index.put(entity, owner, 1, &[vector]).await.unwrap();
    }
    // Earlier, committed: a forget of the owner's and a forget of another's.
    sqlx::query("DELETE FROM proxima_core.embedding_heads WHERE entity_id = ANY($1)")
        .bind(vec![forgotten, foreign])
        .execute(pool)
        .await
        .unwrap();
    let vectors = StoreVectors::Remote(index.clone());

    let mut tx = pool.begin().await.unwrap();
    sqlx::query("DELETE FROM proxima_core.embedding_heads WHERE entity_id = $1")
        .bind(erased)
        .execute(&mut *tx)
        .await
        .unwrap();
    settle_owner_debt(&mut tx, &vectors, owner).await.unwrap();
    let held: Vec<Uuid> = index
        .entries(None, 10)
        .await
        .unwrap()
        .into_iter()
        .map(|entry| entry.key.entity_id)
        .collect();
    assert_eq!(
        held,
        [foreign],
        "the owner's vectors left the index before commit"
    );
    let owed: Vec<Uuid> =
        sqlx::query_scalar("SELECT entity_id FROM proxima_core.vector_index_debt")
            .fetch_all(&mut *tx)
            .await
            .unwrap();
    assert_eq!(owed, [foreign]);
    tx.commit().await.unwrap();
}

/// One transaction's debt reaches the index in calls of a bounded size.
#[tokio::test]
async fn debt_reaches_the_index_in_bounded_calls() {
    #[derive(Default)]
    struct Counting {
        calls: std::sync::Mutex<Vec<usize>>,
    }
    #[async_trait::async_trait]
    impl VectorIndex for Counting {
        async fn put(
            &self,
            _: EmbeddableEntityRef,
            _: OwnerId,
            _: i32,
            _: &[SpaceVector],
        ) -> Result<(), StorageError> {
            Ok(())
        }
        async fn delete(&self, keys: &[VectorKey]) -> Result<(), StorageError> {
            self.calls.lock().unwrap().push(keys.len());
            Ok(())
        }
        async fn set_owner(&self, _: &[VectorKey], _: OwnerId) -> Result<(), StorageError> {
            Ok(())
        }
        async fn nearest(
            &self,
            _: &[OwnerId],
            _: &SpaceVector,
            _: u32,
        ) -> Result<Vec<proxima_core::storage_ports::VectorCandidate>, StorageError> {
            Ok(Vec::new())
        }
        async fn entries(
            &self,
            _: Option<&VectorKey>,
            _: u32,
        ) -> Result<Vec<VectorEntry>, StorageError> {
            Ok(Vec::new())
        }
    }
    let index = Counting::default();
    let id = Uuid::from_u128(1);
    let rows = (0..1_200)
        .map(|i| {
            (
                Uuid::from_u128(i),
                "debt".to_owned(),
                384,
                id,
                "delete".to_owned(),
            )
        })
        .collect();
    apply(&index, rows).await.unwrap();
    assert_eq!(
        *index.calls.lock().unwrap(),
        [KEYS_PER_CALL, KEYS_PER_CALL, 200]
    );
}

/// The sweep leaves a key a drain is writing: a head the index does not
/// hold yet is not deleted while its writer holds the head's lock.
#[tokio::test]
async fn the_sweep_skips_a_head_its_drain_is_writing() {
    let (pg, _guard) = fresh_pg("vector_index_sweep_lock").await;
    let pool = pg.pool_for_tests().clone();
    let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
    sqlx::query("INSERT INTO proxima_core.owners (owner_id, kind) VALUES ($1, 'personal')")
        .bind(owner.into_inner())
        .execute(&pool)
        .await
        .unwrap();
    let (writing, lost) = (Uuid::now_v7(), Uuid::now_v7());
    for entity in [writing, lost] {
        sqlx::query(
            "INSERT INTO proxima_core.embedding_heads (entity_id, model_id, dim, embedding_version, owner_id)
             VALUES ($1, 'debt', 384, 1, $2)",
        )
        .bind(entity)
        .bind(owner.into_inner())
        .execute(&pool)
        .await
        .unwrap();
    }
    let index = InMemoryVectorIndex::default();
    let mut drain = pool.begin().await.unwrap();
    assert!(
        try_lock_embedding_key(&mut drain, writing, "debt", 384)
            .await
            .unwrap()
    );

    let swept = sweep_index(&pg.pool, &pg.platform_scope, &index)
        .await
        .unwrap();
    assert_eq!(swept.heads_deleted, 1, "only the head nobody is writing");
    drain.rollback().await.unwrap();
    let heads: Vec<Uuid> = sqlx::query_scalar("SELECT entity_id FROM proxima_core.embedding_heads")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(heads, [writing]);
}
