//! Integration tests for the codex-auth resolver.
//!
//! Substantive tests land alongside their implementations in later
//! tasks (refresh client tests in Task 4, end-to-end resolver tests
//! in Task 5). This placeholder keeps the wiremock dev-dep wired up
//! so its compilation surface is exercised from the start.

#[tokio::test]
async fn integration_test_harness_compiles() {
    let _ = wiremock::MockServer::start().await;
}
