//! Integration tests for the `persist_wake_trace` verb.

use crate::common::personality::{TEST_PERSPECTIVE_SCHEMA, apply_test_schemas, ingest_test_fact};
use proxima_core::flavor::FlavorRegistry;
use proxima_core::verbs::goal_write::{GoalAuthorship, GoalDraft, GoalState};
use proxima_core::verbs::persist_wake_trace::{WakeTracePayload, WakeTracePersistInput};
use proxima_core::{
    EntityKind, GoalId, MemoryId, OrgId, Owner, OwnerPrincipalKind, Principal, SchemaId,
    SchemaVersion, SourceBatchId, SourceId, Storage, StorageError, UserId, WakeTraceOutcomeKind,
};
use proxima_storage_pg::verbs::persist_wake_trace::persist_wake_trace_atomic;
use uuid::Uuid;

const WAKE_TRACE_JSONL_SCHEMA: &str = "proxima-core/wake-trace-jsonl-v1";

#[tokio::test]
async fn persist_writes_fact_jsonl_citation_sidecars_and_authored_edge() {
    let Some((pg, db_name)) = crate::common::fresh_pg().await else {
        return;
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        apply_test_schemas(pg.pool()).await?;

        let owner = crate::common::owner_fixture();
        let registry = FlavorRegistry::default().freeze();
        let personality_instance_id = Uuid::now_v7();
        let root_p = insert_test_perspective_memory(pg.pool(), &owner).await?;
        let trigger = ingest_test_fact(&pg, &owner, "trigger").await;
        let jsonl: Vec<u8> = b"{\"record\":\"start\"}\n{\"record\":\"finish\"}\n".to_vec();
        let input = sample_persist_input(
            &owner,
            personality_instance_id,
            root_p,
            trigger,
            jsonl.clone(),
        );

        let outcome = persist_wake_trace_atomic(pg.pool(), &registry, &input).await?;

        assert!(!outcome.idempotent_replay);

        let memory_row: (uuid::Uuid,) = sqlx::query_as(
            "SELECT personality_instance_id FROM proxima_core.memories WHERE memory_id = $1",
        )
        .bind(outcome.fact_memory_id.into_inner())
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(memory_row.0, personality_instance_id);

        let sidecar: (uuid::Uuid, WakeTraceOutcomeKind) = sqlx::query_as(
            "SELECT invocation_id, outcome_kind FROM proxima_core.wake_trace_v1 \
             WHERE memory_id = $1",
        )
        .bind(outcome.fact_memory_id.into_inner())
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(sidecar.0, input.wake_trace.invocation_id);
        assert_eq!(sidecar.1, WakeTraceOutcomeKind::Succeeded);

        let jsonl_row: (Vec<u8>, i64) = sqlx::query_as(
            "SELECT body, byte_len FROM proxima_core.cited_wake_trace_jsonl_v1 \
             WHERE cited_object_id = $1",
        )
        .bind(outcome.cited_object_id)
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(jsonl_row.0, jsonl);
        assert_eq!(usize::try_from(jsonl_row.1).unwrap(), jsonl.len());

        let cm_row: (uuid::Uuid, uuid::Uuid) = sqlx::query_as(
            "SELECT memory_id, cited_object_id FROM proxima_core.citation_mappings \
             WHERE citation_mapping_id = $1",
        )
        .bind(outcome.citation_mapping_id)
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(cm_row.0, outcome.fact_memory_id.into_inner());
        assert_eq!(cm_row.1, outcome.cited_object_id);

        let authored: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM proxima_core.edges \
             WHERE relation = 'core/authored' \
               AND source_memory_id = $1 AND target_memory_id = $2",
        )
        .bind(root_p.into_inner())
        .bind(outcome.fact_memory_id.into_inner())
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(authored.0, 1);

        let derived: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM proxima_core.edges \
             WHERE relation = 'core/derived-from' \
               AND source_memory_id = $1 AND target_memory_id = $2",
        )
        .bind(outcome.fact_memory_id.into_inner())
        .bind(trigger.into_inner())
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(derived.0, 1);

        Ok(())
    }
    .await;

    let _ = crate::common::drop_db(&db_name).await;
    result.expect("persist wake trace writes required rows");
}

#[tokio::test]
async fn active_goal_ids_emit_goal_kind_edges_targeting_goal_id() {
    let Some((pg, db_name)) = crate::common::fresh_pg().await else {
        return;
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        apply_test_schemas(pg.pool()).await?;

        let owner = crate::common::owner_fixture();
        let registry = FlavorRegistry::default().freeze();
        let goal_a = insert_test_goal(&pg, &owner, "goal-a").await?;
        let goal_b = insert_test_goal(&pg, &owner, "goal-b").await?;
        let root_p = insert_test_perspective_memory(pg.pool(), &owner).await?;
        let trigger = ingest_test_fact(&pg, &owner, "trigger").await;
        let mut input = sample_persist_input(
            &owner,
            Uuid::now_v7(),
            root_p,
            trigger,
            b"{\"record\":\"start\"}\n".to_vec(),
        );
        input.active_goal_ids = vec![goal_a, goal_b];

        let outcome = persist_wake_trace_atomic(pg.pool(), &registry, &input).await?;

        let goal_edges: Vec<(Option<uuid::Uuid>, Option<uuid::Uuid>, EntityKind)> = sqlx::query_as(
            "SELECT target_memory_id, target_goal_id, target_kind \
             FROM proxima_core.edges \
             WHERE relation = 'core/derived-from' \
               AND source_memory_id = $1 \
               AND target_kind = 'Goal' \
             ORDER BY target_goal_id",
        )
        .bind(outcome.fact_memory_id.into_inner())
        .fetch_all(pg.pool())
        .await?;
        assert_eq!(goal_edges.len(), 2);
        for (target_memory_id, target_goal_id, target_kind) in goal_edges {
            assert_eq!(target_kind, EntityKind::Goal);
            assert!(target_memory_id.is_none());
            assert!(target_goal_id.is_some());
        }

        Ok(())
    }
    .await;

    let _ = crate::common::drop_db(&db_name).await;
    result.expect("goal provenance edges target Goal entities");
}

#[tokio::test]
async fn idempotent_replay_returns_same_ids() {
    let Some((pg, db_name)) = crate::common::fresh_pg().await else {
        return;
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        apply_test_schemas(pg.pool()).await?;

        let owner = crate::common::owner_fixture();
        let registry = FlavorRegistry::default().freeze();
        let root_p = insert_test_perspective_memory(pg.pool(), &owner).await?;
        let trigger = ingest_test_fact(&pg, &owner, "trigger").await;
        let input = sample_persist_input(
            &owner,
            Uuid::now_v7(),
            root_p,
            trigger,
            b"{\"record\":\"start\"}\n".to_vec(),
        );

        let first = persist_wake_trace_atomic(pg.pool(), &registry, &input).await?;
        let second = persist_wake_trace_atomic(pg.pool(), &registry, &input).await?;

        assert!(!first.idempotent_replay);
        assert!(second.idempotent_replay);
        assert_eq!(first.fact_memory_id, second.fact_memory_id);
        assert_eq!(first.cited_object_id, second.cited_object_id);
        assert_eq!(first.citation_mapping_id, second.citation_mapping_id);

        let n_facts: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM proxima_core.wake_trace_v1 WHERE invocation_id = $1",
        )
        .bind(input.wake_trace.invocation_id)
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(n_facts.0, 1);

        Ok(())
    }
    .await;

    let _ = crate::common::drop_db(&db_name).await;
    result.expect("idempotent replay returns existing ids");
}

#[tokio::test]
async fn distinct_invocations_with_identical_jsonl_do_not_collapse() {
    let Some((pg, db_name)) = crate::common::fresh_pg().await else {
        return;
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        apply_test_schemas(pg.pool()).await?;

        let owner = crate::common::owner_fixture();
        let registry = FlavorRegistry::default().freeze();
        let root_p = insert_test_perspective_memory(pg.pool(), &owner).await?;
        let trigger = ingest_test_fact(&pg, &owner, "trigger").await;
        let input_a = sample_persist_input(
            &owner,
            Uuid::now_v7(),
            root_p,
            trigger,
            b"{\"record\":\"start\"}\n".to_vec(),
        );
        let mut input_b = input_a.clone();
        input_b.wake_trace.invocation_id = Uuid::now_v7();

        let a = persist_wake_trace_atomic(pg.pool(), &registry, &input_a).await?;
        let b = persist_wake_trace_atomic(pg.pool(), &registry, &input_b).await?;

        assert!(!a.idempotent_replay);
        assert!(!b.idempotent_replay);
        assert_ne!(a.fact_memory_id, b.fact_memory_id);
        assert_ne!(a.citation_mapping_id, b.citation_mapping_id);
        assert_eq!(a.cited_object_id, b.cited_object_id);

        let n_facts: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM proxima_core.wake_trace_v1")
            .fetch_one(pg.pool())
            .await?;
        assert_eq!(n_facts.0, 2);

        let n_cited: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM proxima_core.cited_objects WHERE schema_id = $1")
                .bind(WAKE_TRACE_JSONL_SCHEMA)
                .fetch_one(pg.pool())
                .await?;
        assert_eq!(n_cited.0, 1);

        Ok(())
    }
    .await;

    let _ = crate::common::drop_db(&db_name).await;
    result.expect("same JSONL across invocations shares only cited object");
}

#[tokio::test]
async fn rejects_jsonl_content_hash_mismatch_before_writing_trace_rows() {
    let Some((pg, db_name)) = crate::common::fresh_pg().await else {
        return;
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        apply_test_schemas(pg.pool()).await?;

        let owner = crate::common::owner_fixture();
        let registry = FlavorRegistry::default().freeze();
        let root_p = insert_test_perspective_memory(pg.pool(), &owner).await?;
        let trigger = ingest_test_fact(&pg, &owner, "trigger").await;
        let mut input = sample_persist_input(
            &owner,
            Uuid::now_v7(),
            root_p,
            trigger,
            b"{\"record\":\"start\"}\n".to_vec(),
        );
        input.jsonl_content_hash = [7; 32];

        let err = persist_wake_trace_atomic(pg.pool(), &registry, &input)
            .await
            .expect_err("mismatched JSONL content hash must be rejected");
        assert!(matches!(
            err,
            StorageError::ConstraintViolation(msg)
                if msg == "wake trace JSONL content hash does not match body"
        ));

        let n_cited: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM proxima_core.cited_objects WHERE schema_id = $1")
                .bind(WAKE_TRACE_JSONL_SCHEMA)
                .fetch_one(pg.pool())
                .await?;
        assert_eq!(n_cited.0, 0);

        let n_wake_traces: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM proxima_core.wake_trace_v1")
                .fetch_one(pg.pool())
                .await?;
        assert_eq!(n_wake_traces.0, 0);

        Ok(())
    }
    .await;

    let _ = crate::common::drop_db(&db_name).await;
    result.expect("JSONL hash mismatch rejected before trace writes");
}

#[tokio::test]
async fn rejects_root_perspective_crossing_owner_boundary() {
    let Some((pg, db_name)) = crate::common::fresh_pg().await else {
        return;
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        apply_test_schemas(pg.pool()).await?;

        let owner = crate::common::owner_fixture();
        let other_owner = other_owner_fixture();
        let registry = FlavorRegistry::default().freeze();
        let other_root_p = insert_test_perspective_memory(pg.pool(), &other_owner).await?;
        let trigger = ingest_test_fact(&pg, &owner, "trigger").await;
        let input = sample_persist_input(
            &owner,
            Uuid::now_v7(),
            other_root_p,
            trigger,
            b"{\"record\":\"start\"}\n".to_vec(),
        );

        let err = persist_wake_trace_atomic(pg.pool(), &registry, &input)
            .await
            .expect_err("cross-owner root perspective must be rejected");
        assert!(matches!(
            err,
            StorageError::ConstraintViolation(msg)
                if msg == "root perspective crosses Owner boundary"
        ));

        let n_wake_traces: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM proxima_core.wake_trace_v1")
                .fetch_one(pg.pool())
                .await?;
        assert_eq!(n_wake_traces.0, 0);

        Ok(())
    }
    .await;

    let _ = crate::common::drop_db(&db_name).await;
    result.expect("cross-owner root perspective rejected");
}

#[tokio::test]
async fn rejects_active_goal_crossing_owner_boundary() {
    let Some((pg, db_name)) = crate::common::fresh_pg().await else {
        return;
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        apply_test_schemas(pg.pool()).await?;

        let owner = crate::common::owner_fixture();
        let other_owner = other_owner_fixture();
        let registry = FlavorRegistry::default().freeze();
        let other_goal = insert_test_goal(&pg, &other_owner, "other-goal").await?;
        let root_p = insert_test_perspective_memory(pg.pool(), &owner).await?;
        let trigger = ingest_test_fact(&pg, &owner, "trigger").await;
        let mut input = sample_persist_input(
            &owner,
            Uuid::now_v7(),
            root_p,
            trigger,
            b"{\"record\":\"start\"}\n".to_vec(),
        );
        input.active_goal_ids = vec![other_goal];

        let err = persist_wake_trace_atomic(pg.pool(), &registry, &input)
            .await
            .expect_err("cross-owner active goal must be rejected");
        assert!(matches!(
            err,
            StorageError::ConstraintViolation(msg)
                if msg == "active goal crosses Owner boundary"
        ));

        let n_wake_traces: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM proxima_core.wake_trace_v1")
                .fetch_one(pg.pool())
                .await?;
        assert_eq!(n_wake_traces.0, 0);

        Ok(())
    }
    .await;

    let _ = crate::common::drop_db(&db_name).await;
    result.expect("cross-owner active goal rejected");
}

#[tokio::test]
async fn rejects_source_batch_id_collision_across_owner_or_source() {
    let Some((pg, db_name)) = crate::common::fresh_pg().await else {
        return;
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        apply_test_schemas(pg.pool()).await?;

        let owner = crate::common::owner_fixture();
        let other_owner = other_owner_fixture();
        let registry = FlavorRegistry::default().freeze();
        let root_p = insert_test_perspective_memory(pg.pool(), &owner).await?;
        let trigger = ingest_test_fact(&pg, &owner, "trigger").await;
        let mut input = sample_persist_input(
            &owner,
            Uuid::now_v7(),
            root_p,
            trigger,
            b"{\"record\":\"start\"}\n".to_vec(),
        );
        insert_test_source_batch(pg.pool(), &other_owner, input.source_batch_id).await?;

        let err = persist_wake_trace_atomic(pg.pool(), &registry, &input)
            .await
            .expect_err("source batch collision must be rejected");
        assert!(matches!(
            err,
            StorageError::ConstraintViolation(msg)
                if msg == "source batch id collides across Owner or source"
        ));

        input.source_batch_id = SourceBatchId::new(Uuid::now_v7());
        let outcome = persist_wake_trace_atomic(pg.pool(), &registry, &input).await?;
        assert!(!outcome.idempotent_replay);

        Ok(())
    }
    .await;

    let _ = crate::common::drop_db(&db_name).await;
    result.expect("source batch collision rejected");
}

fn sample_persist_input(
    owner: &Owner,
    personality_instance_id: Uuid,
    root_perspective_memory_id: MemoryId,
    triggering_memory_id: MemoryId,
    jsonl_bytes: Vec<u8>,
) -> WakeTracePersistInput {
    let now = time::OffsetDateTime::now_utc();
    let content_hash = *blake3::hash(&jsonl_bytes).as_bytes();
    let line_count =
        u64::try_from(jsonl_bytes.split(|b| *b == b'\n').count().saturating_sub(1)).unwrap();
    WakeTracePersistInput {
        owner: owner.clone(),
        authoring_personality_instance_id: personality_instance_id,
        root_perspective_memory_id,
        triggering_memory_id,
        active_goal_ids: vec![],
        jsonl_bytes,
        jsonl_content_hash: content_hash,
        jsonl_line_count: line_count,
        jsonl_truncated: false,
        citation_byte_range: None,
        wake_trace: WakeTracePayload {
            invocation_id: Uuid::now_v7(),
            wake_entry_id: Uuid::now_v7(),
            personality_instance_id,
            model_target_ref: "mistral-default".into(),
            model_id: "mistral-medium-3.5".into(),
            started_at: now,
            finished_at: now,
            outcome_kind: proxima_core::WakeTraceOutcomeKind::Succeeded,
            failure_reason: None,
            rounds_used: 3,
            finish_reason: Some("stop".into()),
            total_prompt_tokens: Some(2048),
            total_completion_tokens: Some(512),
            tool_call_count: 4,
            jsonl_truncated: false,
        },
        source_id: SourceId::new("test/wake-trace"),
        source_batch_id: SourceBatchId::new(Uuid::now_v7()),
        observed_at: now,
        occurred_at: now,
    }
}

fn other_owner_fixture() -> Owner {
    Owner {
        principal: Principal::User(UserId::new(Uuid::now_v7())),
        org_id: OrgId::new(Uuid::now_v7()),
    }
}

async fn insert_test_perspective_memory(
    pool: &sqlx::PgPool,
    owner: &Owner,
) -> Result<MemoryId, sqlx::Error> {
    let memory_id = Uuid::now_v7();
    let owner_kind = OwnerPrincipalKind::of(&owner.principal);
    let owner_principal_id = match &owner.principal {
        Principal::User(u) => u.into_inner(),
        Principal::Group(g) => g.into_inner(),
    };
    sqlx::query(
        "INSERT INTO proxima_core.memories \
            (memory_id, owner_principal_kind, owner_principal_id, owner_org_id, \
             schema_id, schema_version, kind, text, operator_kind, model_id, \
             prompt_version, personality_instance_id) \
         VALUES ($1, $2, $3, $4, $5, 1, 'Perspective', 'Root perspective', \
                 'AtoP', 'test-model', 'test-prompt', $6)",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner.org_id.into_inner())
    .bind(TEST_PERSPECTIVE_SCHEMA)
    .bind(Uuid::now_v7())
    .execute(pool)
    .await?;
    Ok(MemoryId::new(memory_id))
}

async fn insert_test_source_batch(
    pool: &sqlx::PgPool,
    owner: &Owner,
    source_batch_id: SourceBatchId,
) -> Result<(), sqlx::Error> {
    let owner_kind = OwnerPrincipalKind::of(&owner.principal);
    let owner_principal_id = match &owner.principal {
        Principal::User(u) => u.into_inner(),
        Principal::Group(g) => g.into_inner(),
    };
    sqlx::query(
        "INSERT INTO proxima_core.source_batches \
            (id, source_id, owner_principal_kind, owner_principal_id, owner_org_id) \
         VALUES ($1, 'test/other-source', $2, $3, $4)",
    )
    .bind(source_batch_id.into_inner())
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner.org_id.into_inner())
    .execute(pool)
    .await?;
    Ok(())
}

async fn insert_test_goal(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
    title: &str,
) -> Result<GoalId, proxima_core::StorageError> {
    let outcome = pg
        .write_goal_atomic(&GoalDraft {
            principal: owner.principal.clone(),
            org_id: Some(owner.org_id),
            schema_id: SchemaId::new("proxima-test/goal-v1".into()),
            schema_version: SchemaVersion::new(1),
            title: title.into(),
            text: title.into(),
            payload: b"{}".to_vec(),
            state: GoalState::Active,
            parent_goal_ids: vec![],
            supersedes_goal_id: None,
            authorship: GoalAuthorship::User,
            request_id: format!("test-{title}-{}", Uuid::now_v7()),
        })
        .await?;
    Ok(outcome.goal_id)
}
