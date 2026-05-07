//! Provenance auto-wiring. The substrate must materialize a Provenance
//! edge for every read tool call (memory id returned through
//! `core/fetch_memory`) UNIONed with the triggering event. Wake chain
//! depth must equal `max(read.depth, triggering.depth) + 1`.
//!
//! We script the agent to call `core/fetch_memory` once and then
//! `core/emit_perspective` once.

mod common;

use std::sync::Arc;

use common::personality::{
    apply_test_schemas, build_test_engine, ingest_test_fact, instantiate_test_personality,
    TestPerspectiveV1, TestPersonality, TEST_PERSONALITY_TYPE_ID, TEST_PERSPECTIVE_SCHEMA,
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
async fn provenance_includes_triggering_event_and_read_log() {
    let Some((pg, db)) = fresh_pg().await else {
        return;
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        apply_test_schemas(pg.pool()).await?;

        let owner = owner_fixture();

        let earlier_fact_id = ingest_test_fact(&pg, &owner, "earlier-fact").await;

        // Author a Perspective at depth=2 from a different instance to
        // simulate an A2P that the dispatcher will let the wake fetch.
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
        let other_instance = PersonalityRef::new(
            "proxima-test/fake-other-personality-v1",
            proxima_core::personality::PersonalityInstanceId::new(uuid::Uuid::now_v7()),
        );
        let depth_2_outcome = pg
            .append_personality_memories(&PersonalityWriteRequest {
                owner: owner.clone(),
                instance: other_instance,
                model_id: "test-model",
                prompt_version: "test-v1",
                provenance_relation,
                supersedes_relation,
                wake_chain_depth: WakeChainDepth::new(2),
                memories: &[PersonalityMemoryDraft {
                    kind: PersonalityMemoryKind::Perspective,
                    schema_id: SchemaId::new(TEST_PERSPECTIVE_SCHEMA.into()),
                    schema_version: SchemaVersion::new(TestPerspectiveV1::SCHEMA_VERSION),
                    text: "depth-2".into(),
                    typed_payload: serde_json::json!({"label": "depth-2"}),
                    provenance: Vec::new(),
                    embedding: vec![0.0; 8],
                    embedding_model_id: "fake-embed".into(),
                }],
                sidecar_table: "proxima_test.test_perspective_v1",
            })
            .await?;
        let depth_2_id = depth_2_outcome.memory_ids[0];

        // Script the agent: fetch the depth-2 perspective, then emit a
        // new perspective. The triggering Fact is authored AFTER we
        // instantiate the personality so the cursor (parked at "now"
        // on instantiation) sees it.
        let scripted = Arc::new(ScriptedAnthropicClient::new(vec![
            ScriptedTurn::tool_use(
                "core/fetch_memory",
                serde_json::json!({"memory_id": depth_2_id.into_inner()}),
            ),
            ScriptedTurn::tool_use(
                "core/emit_perspective",
                serde_json::json!({
                    "schema_id": TEST_PERSPECTIVE_SCHEMA,
                    "schema_version": 1,
                    "payload": {"label": "wake-output"},
                }),
            ),
            ScriptedTurn::end_turn(),
        ]));
        let engine = build_test_engine(pg.clone(), TestPersonality::new(), scripted);
        let _inst = instantiate_test_personality(&engine, &owner).await?;

        let triggering_fact_id = ingest_test_fact(&pg, &owner, "trigger").await;

        let _ = engine.run_dispatcher_tick().await?;

        let _ = earlier_fact_id;

        // Find the wake-authored Perspective: the most recent
        // Perspective memory authored by this personality whose schema
        // matches the test perspective.
        let row: (uuid::Uuid, i16) = sqlx::query_as(
            "SELECT memory_id, wake_chain_depth
             FROM proxima_core.memories
             WHERE personality_type_id = $1
               AND kind = 'Perspective'
               AND schema_id = $2
             ORDER BY created_at DESC LIMIT 1",
        )
        .bind(TEST_PERSONALITY_TYPE_ID)
        .bind(TEST_PERSPECTIVE_SCHEMA)
        .fetch_one(pg.pool())
        .await?;
        let (wake_memory_id, wake_depth) = row;
        // depth = max(triggering=0, fetched=2) + 1 = 3
        assert_eq!(
            wake_depth, 3,
            "wake_chain_depth must be max(triggering=0, fetched=2)+1 = 3"
        );

        let provenance_targets: Vec<uuid::Uuid> = sqlx::query_scalar(
            "SELECT target_memory_id FROM proxima_core.edges
             WHERE source_memory_id = $1
               AND relation = $2
             ORDER BY target_memory_id",
        )
        .bind(wake_memory_id)
        .bind(CORE_DERIVED_FROM_RELATION)
        .fetch_all(pg.pool())
        .await?;
        let mut expected = vec![triggering_fact_id.into_inner(), depth_2_id.into_inner()];
        expected.sort();
        assert_eq!(
            provenance_targets, expected,
            "provenance must equal triggering ∪ read_log"
        );

        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db).await;
    result.expect("provenance test failed");
}
