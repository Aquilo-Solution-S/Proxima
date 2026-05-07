//! Self-exclusion: a personality must not wake on its own writes.
//!
//! Per spec §Acceptance criteria. Author a Perspective from an Engineer
//! instance via the substrate write path; assert that running a
//! dispatcher tick does NOT record a wake invocation for the resulting
//! change_event seq, and that the cursor advances past it.

mod common;

use std::sync::Arc;

use common::personality::{
    apply_test_schemas, build_test_engine, instantiate_test_personality, TestPersonality,
    TestPerspectiveV1, TEST_PERSONALITY_TYPE_ID, TEST_PERSPECTIVE_SCHEMA,
};
use common::{drop_db, fresh_pg, owner_fixture};
use proxima_core::llm::scripted::{ScriptedAnthropicClient, ScriptedTurn};
use proxima_core::personality::{
    PersonalityMemoryDraft, PersonalityMemoryKind, PersonalityRef, PersonalityWriteRequest,
    WakeChainDepth,
};
use proxima_core::relation::{
    core_relation_descriptors, CORE_DERIVED_FROM_RELATION, CORE_SUPERSEDES_RELATION,
};
use proxima_core::storage::Storage;
use proxima_core::{PerspectivePayload, RegisteredRelation, SchemaId, SchemaVersion};

#[tokio::test(flavor = "multi_thread")]
async fn instance_does_not_wake_on_its_own_writes() {
    let Some((pg, db)) = fresh_pg().await else {
        return;
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        apply_test_schemas(pg.pool()).await?;

        let owner = owner_fixture();
        let scripted = Arc::new(ScriptedAnthropicClient::new(vec![ScriptedTurn::end_turn()]));
        let engine = build_test_engine(pg.clone(), TestPersonality::new(), scripted);
        let inst = instantiate_test_personality(&engine, &owner).await?;
        let instance = PersonalityRef::new(TEST_PERSONALITY_TYPE_ID, inst.instance_id);

        let descriptors = core_relation_descriptors();
        let provenance_descriptor = descriptors
            .iter()
            .find(|d| d.relation == CORE_DERIVED_FROM_RELATION)
            .expect("provenance");
        let supersedes_descriptor = descriptors
            .iter()
            .find(|d| d.relation == CORE_SUPERSEDES_RELATION)
            .expect("supersedes");
        let provenance_relation = RegisteredRelation {
            descriptor: provenance_descriptor,
            payload_sidecar_table: None,
        };
        let supersedes_relation = RegisteredRelation {
            descriptor: supersedes_descriptor,
            payload_sidecar_table: None,
        };

        let draft = PersonalityMemoryDraft {
            kind: PersonalityMemoryKind::Perspective,
            schema_id: SchemaId::new(TEST_PERSPECTIVE_SCHEMA.into()),
            schema_version: SchemaVersion::new(TestPerspectiveV1::SCHEMA_VERSION),
            text: "self-write".into(),
            typed_payload: serde_json::json!({"label": "self-write"}),
            provenance: Vec::new(),
            embedding: vec![0.1, 0.2, 0.3],
            embedding_model_id: "test-embed".into(),
        };
        let outcome = pg
            .append_personality_memories(&PersonalityWriteRequest {
                owner: owner.clone(),
                instance: instance.clone(),
                model_id: "test-model",
                prompt_version: "test-v1",
                provenance_relation,
                supersedes_relation,
                wake_chain_depth: WakeChainDepth::new(0),
                memories: &[draft],
                sidecar_table: "proxima_test.test_perspective_v1",
            })
            .await?;
        assert_eq!(outcome.memory_ids.len(), 1);

        let written_seq: uuid::Uuid = sqlx::query_scalar(
            "SELECT seq FROM proxima_core.change_event
             WHERE entity_memory_id = $1
             ORDER BY seq DESC LIMIT 1",
        )
        .bind(outcome.memory_ids[0].into_inner())
        .fetch_one(pg.pool())
        .await?;

        let _ = engine.run_dispatcher_tick().await?;

        let invocation_for_self: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM proxima_core.personality_wake_invocations
             WHERE personality_type_id = $1
               AND change_event_seq = $2",
        )
        .bind(TEST_PERSONALITY_TYPE_ID)
        .bind(written_seq)
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(
            invocation_for_self, 0,
            "self-exclusion: instance must not wake on its own write"
        );

        let cursor_seq: uuid::Uuid = sqlx::query_scalar(
            "SELECT last_considered_seq
             FROM proxima_core.personality_wake_cursor
             WHERE personality_type_id = $1",
        )
        .bind(TEST_PERSONALITY_TYPE_ID)
        .fetch_one(pg.pool())
        .await?;
        assert!(
            cursor_seq >= written_seq,
            "cursor must advance past self-write events even when no wake fires"
        );

        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db).await;
    result.expect("self-exclusion test failed");
}
