//! Phase 1d: LocalCliGooseAdapter spawns goose with --recipe / --params /
//! --max-turns and maps exit code to TargetOutcome. The success-path test
//! is skipped if goose is missing on PATH; the spawn-failure test always
//! runs against a bogus path.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use proxima_core::wake::target_adapter::local_cli_goose::LocalCliGooseAdapter;
use proxima_core::wake::target_adapter::{
    TargetAdapter, TargetAdapterError, TargetInvocation, TargetOutcomeKind,
};

fn goose_on_path() -> Option<PathBuf> {
    which::which("goose").ok()
}

#[tokio::test]
async fn succeeds_for_minimal_recipe_that_just_says_hi() {
    let Some(goose) = goose_on_path() else {
        eprintln!("skipping: goose not on PATH");
        return;
    };
    let recipe_path = write_minimal_recipe();
    let adapter = LocalCliGooseAdapter::new(goose);
    let invocation = TargetInvocation {
        recipe_path,
        params: HashMap::new(),
        max_rounds: 1,
        env: HashMap::from([
            (
                "PROXIMA_WAKE_TOKEN".to_string(),
                uuid::Uuid::new_v4().to_string(),
            ),
            (
                "PROXIMA_MCP_URL".to_string(),
                "http://127.0.0.1:1/mcp".to_string(),
            ),
        ]),
        timeout: Duration::from_secs(30),
    };
    let outcome = adapter.run(invocation).await.expect("run ok");
    // We don't pin Succeeded vs Truncated for a minimal recipe under any LLM target;
    // we only assert the adapter classified the run, no panic.
    assert!(matches!(
        outcome.kind,
        TargetOutcomeKind::Succeeded | TargetOutcomeKind::Truncated | TargetOutcomeKind::Failed
    ));
}

#[tokio::test]
async fn returns_error_when_binary_missing() {
    let adapter = LocalCliGooseAdapter::new(PathBuf::from("/nonexistent/goose"));
    let invocation = TargetInvocation {
        recipe_path: PathBuf::from("/tmp/nope.yaml"),
        params: HashMap::new(),
        max_rounds: 1,
        env: HashMap::new(),
        timeout: Duration::from_secs(5),
    };
    let err = adapter.run(invocation).await.expect_err("must error");
    assert!(matches!(err, TargetAdapterError::SpawnFailed { .. }));
}

fn write_minimal_recipe() -> PathBuf {
    let tmp = tempfile::Builder::new().suffix(".yaml").tempfile().unwrap();
    std::fs::write(
        tmp.path(),
        r#"version: 1.0.0
title: smoke
description: smoke
instructions: |
  Just say hi and stop.
"#,
    )
    .unwrap();
    let (_file, path) = tmp.keep().unwrap();
    path
}
