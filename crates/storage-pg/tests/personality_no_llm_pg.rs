//! When the engine has no LLM wired (or, in the future, no per-Principal
//! model settings), matched wakes must be **deferred**, not consumed:
//! the cursor stays put and no `wake_invocation` row is created. Once an
//! LLM is configured, the next dispatcher tick fires the deferred wake.

mod common;

use std::sync::Arc;

use proxima_core::auth::NoAuth;
use proxima_core::engine::Engine;
use proxima_core::llm::scripted::{ScriptedAnthropicClient, ScriptedTurn};
use proxima_core::verbs::query::MemoryStore;
use proxima_core::{FlavorRegistry, Principal};

use common::personality::{
    apply_test_schemas, ingest_test_fact, instantiate_test_personality, test_flavor_descriptor,
    FakeEmbedding, TestAbstractionV1, TestFactV1, TestOtherFactV1, TestPersonality,
    TestPersonalitySelfV1, TestPerspectiveV1, TEST_PERSONALITY_TYPE_ID,
};

#[tokio::test(flavor = "multi_thread")]
async fn no_llm_defers_wakes_then_fires_when_llm_is_added() {
    let Some((pg, db)) = common::fresh_pg().await else {
        return;
    };
    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await.expect("core migrations");
        apply_test_schemas(pg.pool()).await?;

        let owner = common::owner_fixture();

        // 1) Engine WITHOUT anthropic. Build the registry directly so we
        //    skip the helper's mandatory `.with_anthropic(...)`.
        let personality = TestPersonality::new();
        let mut registry = FlavorRegistry::new();
        registry.add_flavor(test_flavor_descriptor());
        registry.add_fact_schema::<TestFactV1>();
        registry.add_fact_schema::<TestOtherFactV1>();
        registry.add_perspective_schema::<TestPerspectiveV1>();
        registry.add_perspective_schema::<TestPersonalitySelfV1>();
        registry.add_abstraction_schema::<TestAbstractionV1>();
        registry.add_personality(personality.clone());
        let frozen = registry.freeze();
        let principal: Principal = owner.principal.clone();
        let engine_no_llm = Engine::new(
            frozen,
            MemoryStore::new(),
            Box::new(NoAuth::new(principal, owner.clone())),
        )
        .with_storage(Arc::new(pg.clone()))
        .with_embed(Arc::new(FakeEmbedding { dim: 8 }));

        let inst = instantiate_test_personality(&engine_no_llm, &owner).await?;

        // 2) Ingest a matching fact AFTER instantiation so it lands past
        //    the parked cursor.
        let fact_memory = ingest_test_fact(&pg, &owner, "deferred wake").await;
        let triggering_seq: uuid::Uuid = sqlx::query_scalar(
            "SELECT seq FROM proxima_core.change_event \
             WHERE kind = 'EntityAppend' \
               AND entity_kind = 'Fact' \
               AND entity_memory_id = $1",
        )
        .bind(fact_memory.into_inner())
        .fetch_one(pg.pool())
        .await?;

        // 3) Tick with no LLM. No wake_invocation row should be written,
        //    and the cursor must NOT advance past the matching event.
        let _fired = engine_no_llm.run_dispatcher_tick().await?;
        let invocations: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM proxima_core.personality_wake_invocations \
             WHERE personality_type_id = $1 \
               AND personality_instance_id = $2",
        )
        .bind(TEST_PERSONALITY_TYPE_ID)
        .bind(inst.instance_id.into_inner())
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(
            invocations, 0,
            "no wake_invocation row should be inserted when LLM is missing"
        );
        let cursor: uuid::Uuid = sqlx::query_scalar(
            "SELECT last_considered_seq FROM proxima_core.personality_wake_cursor \
             WHERE personality_type_id = $1 \
               AND personality_instance_id = $2",
        )
        .bind(TEST_PERSONALITY_TYPE_ID)
        .bind(inst.instance_id.into_inner())
        .fetch_one(pg.pool())
        .await?;
        assert!(
            cursor < triggering_seq,
            "cursor must remain before the matching event when LLM is missing \
             (cursor={cursor}, triggering={triggering_seq})"
        );

        // 4) Now wire an LLM and tick again. The deferred wake must fire.
        let scripted = Arc::new(ScriptedAnthropicClient::new(vec![ScriptedTurn::end_turn()]));
        let mut registry = FlavorRegistry::new();
        registry.add_flavor(test_flavor_descriptor());
        registry.add_fact_schema::<TestFactV1>();
        registry.add_fact_schema::<TestOtherFactV1>();
        registry.add_perspective_schema::<TestPerspectiveV1>();
        registry.add_perspective_schema::<TestPersonalitySelfV1>();
        registry.add_abstraction_schema::<TestAbstractionV1>();
        registry.add_personality(personality);
        let frozen = registry.freeze();
        let engine_with_llm = Engine::new(
            frozen,
            MemoryStore::new(),
            Box::new(NoAuth::new(owner.principal.clone(), owner.clone())),
        )
        .with_storage(Arc::new(pg.clone()))
        .with_anthropic(scripted)
        .with_embed(Arc::new(FakeEmbedding { dim: 8 }));

        let _ = engine_with_llm.run_dispatcher_tick().await?;
        let invocations: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM proxima_core.personality_wake_invocations \
             WHERE personality_type_id = $1 \
               AND personality_instance_id = $2 \
               AND change_event_seq = $3",
        )
        .bind(TEST_PERSONALITY_TYPE_ID)
        .bind(inst.instance_id.into_inner())
        .bind(triggering_seq)
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(
            invocations, 1,
            "wake must fire once an LLM is wired (deferred event was preserved)"
        );

        Ok(())
    }
    .await;

    drop(pg);
    let _ = common::drop_db(&db).await;
    result.expect("no-llm defer test failed");
}
