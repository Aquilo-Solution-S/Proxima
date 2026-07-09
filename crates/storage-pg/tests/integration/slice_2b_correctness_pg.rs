//! Slice 2B correctness regressions.
//!
//! K3: two concurrent same-idempotency-key writes must both succeed — one
//! original, one idempotent replay — instead of the loser surfacing a spurious
//! unique-violation. The loser collides mid-transaction on the receipt / goal
//! idempotency-key unique index; the SAVEPOINT added in this slice lets it roll
//! back and replay the winner's committed row.

use std::sync::Arc;

use crate::common::{create_db, db_url, drop_db, fresh_pg, owner_write_permit};

use proxima_core::authz::AuthPath;
use proxima_core::verbs::fact_ingest::{FactReceiptDraft, FactWriteCommand};
use proxima_core::verbs::goal_write::{GoalAssignmentTarget, GoalCreateRequest, IdempotencyKey};
use proxima_core::{
    AccessKind, AuthzContext, Engine, GoalPayload, GroupId, MemoryId, Owner, OwnerRef,
    PayloadKeyBuilder, Role, SchemaId, SchemaVersion, SourceBatchId, SourceId, UserId,
};
use proxima_storage_pg::PgStorage;
use proxima_storage_pg::verbs::fact_ingest::ingest_fact_atomic;
use uuid::Uuid;

fn race_draft() -> FactWriteCommand {
    // A fixed payload + source id ⇒ a stable receipt id (receipt identity is
    // batch-independent), so two ingests race for the same
    // `fact_receipts_pkey` / `memories_one_fact_per_receipt` key. Each call
    // gets its own batch id, matching the realistic re-run/replay path.
    let now = time::OffsetDateTime::now_utc();
    FactWriteCommand {
        schema_id: SchemaId::new("test/fact_blob".into()),
        schema_version: SchemaVersion::new(1),
        payload: b"k3-fixed-receipt-payload".to_vec(),
        rendered_text: None,
        receipt: Some(FactReceiptDraft {
            source_id: SourceId::new("test/source"),
            source_batch_id: SourceBatchId::new(Uuid::now_v7()),
            observed_at: now,
            occurred_at: now,
        }),
        citation: None,
    }
}

#[tokio::test]
async fn concurrent_same_receipt_fact_ingests_replay_without_error() {
    let (pg, db_name) = fresh_pg().await;

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = owner_write_permit(&owner, AccessKind::Fact).await?;
        // Same payload/source (⇒ same receipt), distinct batch ids per call.
        let draft_a = race_draft();
        let draft_b = race_draft();
        let pool = pg.pool_for_tests();

        // Two concurrent ingests of the identical receipt. Whichever loses the
        // `fact_receipts_pkey` race must replay the winner's Fact, not error.
        let (first, second) = tokio::join!(
            ingest_fact_atomic(pool, &permit, &draft_a, None),
            ingest_fact_atomic(pool, &permit, &draft_b, None),
        );
        let first = first?;
        let second = second?;

        assert_eq!(
            first.memory_id, second.memory_id,
            "both ingests must resolve the same Fact memory"
        );
        assert_eq!(first.change_event_seq, second.change_event_seq);
        assert_eq!(first.receipt_id, second.receipt_id);
        assert!(
            first.idempotent_replay ^ second.idempotent_replay,
            "exactly one original + one replay (first={}, second={})",
            first.idempotent_replay,
            second.idempotent_replay,
        );

        let memory_rows: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM proxima_core.memories WHERE receipt_id IS NOT NULL",
        )
        .fetch_one(pool)
        .await?;
        assert_eq!(memory_rows, 1, "exactly one Fact row must persist");
        let receipt_rows: i64 =
            sqlx::query_scalar("SELECT count(*) FROM proxima_core.fact_receipts")
                .fetch_one(pool)
                .await?;
        assert_eq!(receipt_rows, 1, "exactly one receipt row must persist");
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("K3 concurrent fact ingest replay failed");
}

const GOAL_REQUEST_ID: &str = "k3:goal:same-key:1";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct RaceGoal {
    external_goal_id: String,
}

impl GoalPayload for RaceGoal {
    const SCHEMA_ID: &'static str = "test/k3-race-goal-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn goal_key(&self) -> Vec<u8> {
        let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
        key.field_str("external_goal_id", &self.external_goal_id);
        key.finish()
    }
}

async fn insert_self_perspective(pg: &PgStorage, owner: &Owner) -> Result<MemoryId, sqlx::Error> {
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(owner);
    let memory_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_kind, owner_id, schema_id, schema_version, kind, text,
             operator_kind, operator_id, input_contract_id, source_batch_id, model_id, prompt_version)
         VALUES ($1, $2, $3, 'test/k3-self', 1, 'Perspective', 'self',
                 'AtoP',
                 '00000000-0000-0000-0000-000000000341'::uuid,
                 '00000000-0000-0000-0000-000000000342'::uuid,
                 NULL, 'test-model', 'v1')",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .execute(pg.pool_for_tests())
    .await?;
    Ok(MemoryId::new(memory_id))
}

fn goal_request(owner: &Owner, target_self: MemoryId) -> GoalCreateRequest<RaceGoal> {
    GoalCreateRequest::product(
        *owner,
        GoalAssignmentTarget::perspective(target_self),
        IdempotencyKey::new(GOAL_REQUEST_ID).expect("stable request id is valid"),
        "Race goal",
        "goal body",
        RaceGoal {
            external_goal_id: "k3-race".to_string(),
        },
    )
}

#[tokio::test]
async fn concurrent_same_key_goal_creates_replay_without_error() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let owner = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
        let target_self = insert_self_perspective(&pg, &owner).await?;
        let engine =
            Engine::compose_or_panic_for_tests(Arc::new(pg.clone()).storage_ports(), |registry| {
                registry.add_goal_schema_or_panic_for_tests::<RaceGoal>();
            });
        let authz = AuthzContext::for_subject_with_role(
            UserId::new(Uuid::now_v7()),
            [(owner, Role::admin())],
            AuthPath::HostBearer,
        );

        let (first, second) = tokio::join!(
            engine.create_goal(&authz, goal_request(&owner, target_self)),
            engine.create_goal(&authz, goal_request(&owner, target_self)),
        );
        let first = first?;
        let second = second?;

        assert_eq!(
            first.goal_id, second.goal_id,
            "both creates must resolve the same goal"
        );
        assert_eq!(first.change_event_seq, second.change_event_seq);
        assert!(
            first.idempotent_replay ^ second.idempotent_replay,
            "exactly one original + one replay (first={}, second={})",
            first.idempotent_replay,
            second.idempotent_replay,
        );

        let goal_rows: i64 = sqlx::query_scalar("SELECT count(*) FROM proxima_core.goals")
            .fetch_one(pg.pool_for_tests())
            .await?;
        assert_eq!(goal_rows, 1, "exactly one goal row must persist");
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("K3 concurrent goal create replay failed");
}
