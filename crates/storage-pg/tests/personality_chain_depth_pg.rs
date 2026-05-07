//! Wake-chain depth cap. Per spec §Acceptance criteria: when a
//! personality's `max_wake_chain_depth()` is exceeded, the dispatcher
//! must NOT fire a wake on the offending event but must still advance
//! the cursor past it.
//!
//! We use a TestPersonality with `max_wake_chain_depth = 3` and
//! manually fabricate a chain of personality memories at depths 0..=3
//! (one Fact + three personality writes). The dispatcher tick should
//! refuse to fire on the depth=3 row (it equals the cap).

mod common;

use std::sync::Arc;

use common::personality::{
    apply_test_schemas, build_test_engine, ingest_test_fact, instantiate_test_personality,
    TestPerspectiveV1, TestPersonality, TEST_PERSONALITY_TYPE_ID, TEST_PERSPECTIVE_SCHEMA,
};
use common::{drop_db, fresh_pg, owner_fixture};
use proxima_core::llm::scripted::{ScriptedAnthropicClient, ScriptedTurn};
use proxima_core::personality::{
    PersonalityInstanceId, PersonalityMemoryDraft, PersonalityMemoryKind, PersonalityRef,
    PersonalityWriteRequest, WakeChainDepth,
};
use proxima_core::relation::{
    core_relation_descriptors, CORE_DERIVED_FROM_RELATION, CORE_SUPERSEDES_RELATION,
};
use proxima_core::storage::Storage;
use proxima_core::{PerspectivePayload, RegisteredRelation, SchemaId, SchemaVersion};
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread")]
async fn dispatcher_skips_wake_when_chain_depth_hits_cap() {
    let Some((pg, db)) = fresh_pg().await else {
        return;
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        apply_test_schemas(pg.pool()).await?;

        let owner = owner_fixture();
        let scripted = Arc::new(ScriptedAnthropicClient::new(vec![ScriptedTurn::end_turn()]));
        let engine = build_test_engine(
            pg.clone(),
            TestPersonality::new().with_max_depth(3),
            scripted,
        );
        let inst = instantiate_test_personality(&engine, &owner).await?;
        // A second instance authors the depth-2 chain so the depth-3
        // event isn't excluded by self-exclusion against `inst`. The
        // dispatcher's depth cap kicks in independently.
        let other_instance_id = PersonalityInstanceId::new(Uuid::now_v7());

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

        let fact_id = ingest_test_fact(&pg, &owner, "depth-0").await;

        // Author chained Perspectives at depths 1, 2, 3. Author from
        // `other_instance_id` so self-exclusion doesn't drop the
        // depth=3 event before the depth check runs.
        let mut last = fact_id;
        let mut depth_3_seq: Option<uuid::Uuid> = None;
        for depth in 1..=3u16 {
            let other = PersonalityRef::new(TEST_PERSONALITY_TYPE_ID, other_instance_id);
            let draft = PersonalityMemoryDraft {
                kind: PersonalityMemoryKind::Perspective,
                schema_id: SchemaId::new(TEST_PERSPECTIVE_SCHEMA.into()),
                schema_version: SchemaVersion::new(TestPerspectiveV1::SCHEMA_VERSION),
                text: format!("depth-{depth}"),
                typed_payload: serde_json::json!({"label": format!("depth-{depth}")}),
                provenance: vec![last],
                embedding: vec![0.1, 0.2, 0.3],
                embedding_model_id: "test-embed".into(),
            };
            let outcome = pg
                .append_personality_memories(&PersonalityWriteRequest {
                    owner: owner.clone(),
                    instance: other,
                    model_id: "test-model",
                    prompt_version: "test-v1",
                    provenance_relation,
                    supersedes_relation,
                    wake_chain_depth: WakeChainDepth::new(depth),
                    memories: &[draft],
                    sidecar_table: "proxima_test.test_perspective_v1",
                })
                .await?;
            last = outcome.memory_ids[0];
            let seq: uuid::Uuid = sqlx::query_scalar(
                "SELECT seq FROM proxima_core.change_event
                 WHERE entity_memory_id = $1
                 ORDER BY seq DESC LIMIT 1",
            )
            .bind(last.into_inner())
            .fetch_one(pg.pool())
            .await?;
            if depth == 3 {
                depth_3_seq = Some(seq);
            }
        }
        let depth_3_seq = depth_3_seq.expect("captured depth-3 seq");

        // Run dispatcher. The configured personality has max_depth = 3,
        // so the dispatcher's `should_skip_self_or_depth` refuses any
        // event whose depth >= 3.
        let _ = engine.run_dispatcher_tick().await?;

        // Assert no wake_invocation row recorded for the depth-3 seq —
        // even though it matched the wake_filter (Perspective).
        // (Perspectives at depth 1/2 are not in the filter — only
        // Facts. So they were skipped due to no match. The depth-3 was
        // skipped due to depth cap. Either way, zero rows for `inst`.)
        let invocation_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM proxima_core.personality_wake_invocations
             WHERE personality_type_id = $1
               AND personality_instance_id = $2",
        )
        .bind(TEST_PERSONALITY_TYPE_ID)
        .bind(inst.instance_id.into_inner())
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(
            invocation_count, 1,
            "dispatcher fires once on the Fact (depth=0) and skips the depth-3 row"
        );

        let cursor_seq: uuid::Uuid = sqlx::query_scalar(
            "SELECT last_considered_seq
             FROM proxima_core.personality_wake_cursor
             WHERE personality_type_id = $1
               AND personality_instance_id = $2",
        )
        .bind(TEST_PERSONALITY_TYPE_ID)
        .bind(inst.instance_id.into_inner())
        .fetch_one(pg.pool())
        .await?;
        assert!(
            cursor_seq >= depth_3_seq,
            "cursor must advance past the depth-3 event regardless of cap-skip"
        );

        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db).await;
    result.expect("chain_depth test failed");
}
