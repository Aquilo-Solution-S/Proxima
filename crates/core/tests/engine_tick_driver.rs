//! Phase 1d Task 11: starting the engine spins the dispatcher loop.
//! Seed events, start engine, observe invocation rows after one tick
//! interval — confirms `Engine::start` autonomously fires the
//! dispatcher without a caller invoking `run_dispatcher_tick` directly.

mod common;

use std::time::Duration;

#[tokio::test(flavor = "multi_thread")]
async fn dispatcher_loop_fires_after_start() {
    let Some(fixture) =
        common::seed_dispatch_fixture_with_match_and_engine(Duration::from_millis(100)).await
    else {
        panic!("PG required for tests but unavailable");
    };
    let engine = fixture.engine.clone();

    let handle = engine.clone().start().await.expect("start");

    // Two ticks worth of margin past the 100ms interval.
    tokio::time::sleep(Duration::from_millis(350)).await;

    let n = fixture.count_invocation_rows().await;
    assert!(n >= 1, "expected >=1 invocation row, got {n}");

    engine.stop(handle).await;

    fixture.cleanup().await;
}
