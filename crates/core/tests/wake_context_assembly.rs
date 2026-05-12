//! Phase 1d: assemble_wake_context returns the four fixed params (spec
//! lines 285-306).

mod common;

use proxima_core::SchemaId;
use proxima_core::personality::SidecarSpec;
use proxima_core::wake::context::assemble_wake_context;

#[tokio::test(flavor = "multi_thread")]
async fn assembles_all_four_params() {
    let Some((storage, owner, instance_id, change_event_seq, fixture)) =
        common::seed_wake_context_fixture().await
    else {
        eprintln!("skipping: PG unavailable");
        return;
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
