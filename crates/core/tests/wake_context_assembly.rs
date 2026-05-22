//! Phase 1d: assemble_wake_context returns the four fixed params (spec
//! lines 285-306).

mod common;

use proxima_core::SchemaId;
use proxima_core::personality::SidecarSpec;
use proxima_core::wake::context::assemble_wake_context;
use sqlx::Executor;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread")]
async fn assembles_all_four_params() {
    let Some((storage, owner, instance_id, change_event_seq, fixture)) =
        common::seed_wake_context_fixture().await
    else {
        panic!("PG required for tests but unavailable");
    };

    let sidecars = vec![SidecarSpec {
        schema_id: SchemaId::new("proxima-test/wake-context-fact-v1".into()),
        sidecar_table: "proxima_test.wake_context_fact_v1".into(),
    }];

    let ctx = assemble_wake_context(
        storage.as_ref(),
        &owner,
        instance_id,
        change_event_seq,
        &sidecars,
    )
    .await
    .expect("assemble ok");

    assert!(!ctx.root_perspective.display_name.is_empty());
    assert!(!ctx.root_perspective.system_prompt.is_empty());
    assert_eq!(ctx.trigger_event.change_event_seq, change_event_seq);
    assert_ne!(ctx.triggering_memory.memory_id, uuid::Uuid::nil());
    // active_goals may be empty; just assert the type / shape.
    let _: &Vec<_> = &ctx.active_goals;

    // Harness programs get the run-time instance id back so they can
    // disambiguate sibling personalities; sanity-check the round-trip.
    assert_eq!(ctx.root_perspective.instance_id, instance_id.into_inner());

    // Triggering memory carries the typed sidecar payload — the
    // fixture's Fact wrote `label = "wake-context-test-trigger"`.
    let label = ctx
        .triggering_memory
        .typed_payload
        .get("label")
        .and_then(|v| v.as_str());
    assert_eq!(label, Some("wake-context-test-trigger"));

    fixture.cleanup().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn injects_active_learned_perspectives_after_root() {
    let Some((storage, owner, instance_id, change_event_seq, fixture)) =
        common::seed_wake_context_fixture().await
    else {
        panic!("PG required for tests but unavailable");
    };

    let perspective_id = Uuid::now_v7();
    let owner_principal_id = match &owner.principal {
        proxima_core::Principal::User(id) => id.into_inner(),
        proxima_core::Principal::Group(id) => id.into_inner(),
    };
    fixture
        .pg
        .pool()
        .execute(
            sqlx::query(
                "INSERT INTO proxima_core.memories
                    (memory_id, owner_principal_kind, owner_principal_id, owner_org_id,
                     schema_id, schema_version, kind, text, operator_kind, model_id,
                     prompt_version, personality_instance_id, wake_chain_depth)
                 VALUES ($1, 'User', $2, $3, 'proxima-test/wake-context-perspective-v1',
                         1, 'Perspective', $4, 'Wake', 'test-model', 'test-v1', $5, 2)",
            )
            .bind(perspective_id)
            .bind(owner_principal_id)
            .bind(owner.org_id.into_inner())
            .bind("remember to inspect workspace before writing")
            .bind(instance_id.into_inner()),
        )
        .await
        .expect("insert perspective memory");
    fixture
        .pg
        .pool()
        .execute(
            sqlx::query(
                "INSERT INTO proxima_test.wake_context_perspective_v1 (memory_id, label)
                 VALUES ($1, $2)",
            )
            .bind(perspective_id)
            .bind("learned"),
        )
        .await
        .expect("insert perspective sidecar");

    let sidecars = vec![
        SidecarSpec {
            schema_id: SchemaId::new("proxima-test/wake-context-fact-v1".into()),
            sidecar_table: "proxima_test.wake_context_fact_v1".into(),
        },
        SidecarSpec {
            schema_id: SchemaId::new("proxima-test/wake-context-perspective-v1".into()),
            sidecar_table: "proxima_test.wake_context_perspective_v1".into(),
        },
    ];

    let ctx = assemble_wake_context(
        storage.as_ref(),
        &owner,
        instance_id,
        change_event_seq,
        &sidecars,
    )
    .await
    .expect("assemble ok");

    assert_eq!(ctx.active_perspectives.len(), 1);
    assert_eq!(ctx.active_perspectives[0].memory_id, perspective_id);
    assert_eq!(
        ctx.active_perspectives[0].text,
        "remember to inspect workspace before writing"
    );

    fixture.cleanup().await;
}
