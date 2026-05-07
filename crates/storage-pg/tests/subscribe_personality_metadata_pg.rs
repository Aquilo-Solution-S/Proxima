//! Verifies that personality metadata authored on a `change_event` row
//! flows through the outbox/subscribe path — i.e., that the
//! `ChangeEvent` value the gRPC `Subscribe` stream emits carries
//! `authoring_personality_type_id`, `authoring_personality_instance_id`,
//! and `wake_chain_depth` populated.
//!
//! Two assertions:
//! 1. External-source ingestion → fields are `None`/`0`.
//! 2. Personality-authored entity (a Perspective written by a wake) →
//!    fields are populated with the personality's `(type_id,
//!    instance_id)` and a non-zero `wake_chain_depth`.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::personality::{
    apply_test_schemas, ingest_test_fact, instantiate_test_personality, test_flavor_descriptor,
    FakeEmbedding, TestAbstractionV1, TestFactV1, TestOtherFactV1, TestPersonality,
    TestPersonalitySelfV1, TestPerspectiveV1, TEST_PERSONALITY_TYPE_ID, TEST_PERSPECTIVE_SCHEMA,
};
use proxima_core::auth::{Credentials, NoAuth};
use proxima_core::engine::Engine;
use proxima_core::llm::scripted::{ScriptedAnthropicClient, ScriptedTurn};
use proxima_core::storage::Storage;
use proxima_core::verbs::query::MemoryStore;
use proxima_core::verbs::subscribe::SubscribeRequest;
use proxima_core::{ChangeEventKind, EntityKind, FlavorRegistry, PerspectivePayload, SchemaId};
use tokio_stream::StreamExt;

#[tokio::test(flavor = "multi_thread")]
async fn subscribe_carries_personality_metadata() {
    let Some((pg, db)) = common::fresh_pg().await else {
        return;
    };
    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        apply_test_schemas(pg.pool()).await?;
        pg.start_outbox().await?;

        let owner = common::owner_fixture();

        // Build engine with the test personality + scripted Anthropic
        // (one tool_use turn that writes a Perspective, then end_turn).
        let scripted = Arc::new(ScriptedAnthropicClient::new(vec![
            ScriptedTurn::tool_use(
                "core/emit_perspective",
                serde_json::json!({
                    "schema_id": <TestPerspectiveV1 as PerspectivePayload>::SCHEMA_ID,
                    "schema_version": 1,
                    "payload": { "label": "wake-output" },
                }),
            ),
            ScriptedTurn::end_turn(),
        ]));
        let mut registry = FlavorRegistry::new();
        registry.add_flavor(test_flavor_descriptor());
        registry.add_fact_schema::<TestFactV1>();
        registry.add_fact_schema::<TestOtherFactV1>();
        registry.add_perspective_schema::<TestPerspectiveV1>();
        registry.add_perspective_schema::<TestPersonalitySelfV1>();
        registry.add_abstraction_schema::<TestAbstractionV1>();
        registry.add_personality(TestPersonality::new());
        let frozen = registry.freeze();
        let storage: Arc<dyn Storage> = Arc::new(pg.clone());
        let engine = Engine::new(
            frozen,
            MemoryStore::new(),
            Box::new(NoAuth::new(owner.principal.clone(), owner.clone())),
        )
        .with_storage(storage.clone())
        .with_anthropic(scripted)
        .with_embed(Arc::new(FakeEmbedding { dim: 8 }));

        // Subscribe BEFORE any writes so we see the entire history.
        let req = SubscribeRequest {
            owner: owner.clone(),
            since: None,
        };
        let mut stream = engine.subscribe(&Credentials::None, req).await?;

        let inst = instantiate_test_personality(&engine, &owner).await?;

        // Ingest a fact (external source) — should appear with
        // `authoring_personality_*` = None.
        let fact_memory = ingest_test_fact(&pg, &owner, "metadata trigger").await;

        // Tick the dispatcher — the personality wakes and writes a
        // Perspective.
        let _ = engine.run_dispatcher_tick().await?;

        // Drain Subscribe stream until we see both events. We expect:
        //   1. The Fact (entity_personality = sentinel → None on wire).
        //   2. The Perspective from the wake (authoring = test
        //      personality, depth = 1).
        // The outbox may also publish other events (citation/cited
        // edges or auto-wired derived_from) — we tolerate those.
        let mut saw_fact = false;
        let mut saw_perspective = false;
        let deadline = std::time::Instant::now() + Duration::from_secs(5);

        while !(saw_fact && saw_perspective) {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            let Ok(Some(event)) = tokio::time::timeout(remaining, stream.next()).await else {
                break;
            };
            if let ChangeEventKind::EntityAppend {
                entity_kind,
                entity,
                schema_id,
                ..
            } = &event.kind
            {
                if *entity_kind == EntityKind::Fact
                    && *entity == proxima_core::EntityRef::Memory(fact_memory)
                {
                    assert_eq!(
                        event.authoring_personality_type_id, None,
                        "external-source fact must surface authoring_personality_type_id = None"
                    );
                    assert_eq!(event.authoring_personality_instance_id, None);
                    assert_eq!(event.wake_chain_depth, 0);
                    saw_fact = true;
                } else if *entity_kind == EntityKind::Perspective
                    && schema_id == &SchemaId::new(TEST_PERSPECTIVE_SCHEMA.into())
                {
                    assert_eq!(
                        event.authoring_personality_type_id.as_deref(),
                        Some(TEST_PERSONALITY_TYPE_ID),
                        "wake-authored perspective must carry the personality type_id"
                    );
                    assert_eq!(
                        event.authoring_personality_instance_id,
                        Some(inst.instance_id.into_inner())
                    );
                    assert!(
                        event.wake_chain_depth >= 1,
                        "wake-authored event must have wake_chain_depth >= 1, got {}",
                        event.wake_chain_depth
                    );
                    saw_perspective = true;
                }
            }
        }

        assert!(saw_fact, "did not observe the external fact via Subscribe");
        assert!(
            saw_perspective,
            "did not observe the wake-authored perspective via Subscribe"
        );
        Ok(())
    }
    .await;

    drop(pg);
    let _ = common::drop_db(&db).await;
    result.expect("subscribe_personality_metadata test failed");
}
