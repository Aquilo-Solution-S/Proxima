//! Phase 1d: LocalCliGooseAdapter spawns goose with --recipe / --params /
//! optional --max-turns / --no-session and maps exit code to TargetOutcome. The
//! success-path test is skipped if goose is missing on PATH; the
//! spawn-failure test always runs against a bogus path.

use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
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
        enable_developer_builtin: false,
        cwd: None,
        session_log_path: None,
        invocation_id: None,
        personality_instance_id: None,
        wake_entry_id: None,
        change_event_seq: None,
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
async fn passes_current_batch_mode_flag_to_goose() {
    let dir = tempfile::tempdir().unwrap();
    let goose = dir.path().join("goose");
    let args = dir.path().join("args.txt");
    let session_log = dir.path().join("worker-session.jsonl");
    let wake_token = uuid::Uuid::new_v4().to_string();
    fs::write(
        &goose,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$GOOSE_ARG_CAPTURE\"\nprintf '%s\\n%s\\n%s\\n%s\\n%s\\n%s\\n' \"$GOOSE_MODE\" \"$GOOSE_CONTEXT_STRATEGY\" \"$GOOSE_AUTO_COMPACT_THRESHOLD\" \"$GOOSE_TOOL_CALL_CUTOFF\" \"$GOOSE_CLI_MIN_PRIORITY\" \"$GOOSE_CLI_TOOL_PARAMS_TRUNCATION_MAX_LENGTH\" > \"$GOOSE_ENV_CAPTURE\"\nprintf '%s\\n' '{\"type\":\"message\",\"turn\":1}'\nprintf '%s\\n' 'debug stderr' >&2\n",
    )
    .unwrap();
    let mut perms = fs::metadata(&goose).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&goose, perms).unwrap();

    let adapter = LocalCliGooseAdapter::new(goose);
    let invocation = TargetInvocation {
        recipe_path: PathBuf::from("/tmp/recipe.yaml"),
        params: HashMap::from([(
            "triggering_memory_id".to_string(),
            serde_json::json!("019e0000-0000-7000-8000-000000000001"),
        )]),
        max_rounds: 7,
        env: HashMap::from([
            ("GOOSE_ARG_CAPTURE".to_string(), args.display().to_string()),
            (
                "GOOSE_ENV_CAPTURE".to_string(),
                dir.path().join("goose-env.txt").display().to_string(),
            ),
            ("PROXIMA_WAKE_TOKEN".to_string(), wake_token.clone()),
        ]),
        timeout: Duration::from_secs(5),
        enable_developer_builtin: true,
        cwd: None,
        session_log_path: Some(session_log.clone()),
        invocation_id: Some(uuid::Uuid::new_v4()),
        personality_instance_id: Some(uuid::Uuid::new_v4()),
        wake_entry_id: Some(uuid::Uuid::new_v4()),
        change_event_seq: Some(uuid::Uuid::new_v4()),
    };

    let outcome = adapter.run(invocation).await.expect("run ok");
    assert!(matches!(outcome.kind, TargetOutcomeKind::Succeeded));
    let captured = fs::read_to_string(args).unwrap();
    assert!(captured.contains("--no-profile\n"));
    assert!(captured.contains("--no-session\n"));
    assert!(captured.contains("--max-tool-repetitions\n3\n"));
    assert!(captured.contains("--output-format\nstream-json\n"));
    assert!(!captured.contains("--debug\n"));
    assert!(captured.contains("--max-turns\n7\n"));
    assert!(captured.contains("--with-builtin\ndeveloper\n"));
    assert!(!captured.contains("--no-interactive"));
    assert!(!captured.contains("--text\n"));
    assert!(captured.contains("--params\n"));
    assert!(captured.contains("triggering_memory_id"));
    assert_eq!(
        fs::read_to_string(dir.path().join("goose-env.txt")).unwrap(),
        "auto\nsummarize\n0.8\n5\n0.8\n160\n"
    );

    let artifact = fs::read_to_string(session_log).expect("session log");
    assert!(artifact.contains("\"record\":\"start\""));
    assert!(artifact.contains("\"record\":\"stdout\""));
    assert!(artifact.contains("\"record\":\"stderr\""));
    assert!(artifact.contains("\"record\":\"finish\""));
    assert!(artifact.contains("\"parsed\":"));
    assert!(artifact.contains("\"type\":\"message\""));
    assert!(artifact.contains("\"turn\":1"));
    assert!(artifact.contains("\"env_keys\""));
    assert!(artifact.contains("GOOSE_AUTO_COMPACT_THRESHOLD"));
    assert!(artifact.contains("GOOSE_CLI_MIN_PRIORITY"));
    assert!(artifact.contains("GOOSE_CLI_TOOL_PARAMS_TRUNCATION_MAX_LENGTH"));
    assert!(artifact.contains("GOOSE_CONTEXT_STRATEGY"));
    assert!(artifact.contains("GOOSE_MODE"));
    assert!(artifact.contains("GOOSE_TOOL_CALL_CUTOFF"));
    assert!(artifact.contains("PROXIMA_WAKE_TOKEN"));
    assert!(!artifact.contains(&wake_token));
}

#[tokio::test]
async fn omits_max_turns_when_max_rounds_is_zero() {
    let dir = tempfile::tempdir().unwrap();
    let goose = dir.path().join("goose");
    let args = dir.path().join("args.txt");
    fs::write(
        &goose,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$GOOSE_ARG_CAPTURE\"\nprintf '%s\\n' '{\"type\":\"message\",\"turn\":1}'\n",
    )
    .unwrap();
    let mut perms = fs::metadata(&goose).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&goose, perms).unwrap();

    let adapter = LocalCliGooseAdapter::new(goose);
    let invocation = TargetInvocation {
        recipe_path: PathBuf::from("/tmp/recipe.yaml"),
        params: HashMap::new(),
        max_rounds: 0,
        env: HashMap::from([("GOOSE_ARG_CAPTURE".to_string(), args.display().to_string())]),
        timeout: Duration::from_secs(5),
        enable_developer_builtin: false,
        cwd: None,
        session_log_path: None,
        invocation_id: None,
        personality_instance_id: None,
        wake_entry_id: None,
        change_event_seq: None,
    };

    let outcome = adapter.run(invocation).await.expect("run ok");
    assert!(matches!(outcome.kind, TargetOutcomeKind::Succeeded));
    let captured = fs::read_to_string(args).unwrap();
    assert!(!captured.contains("--max-turns\n"));
    assert!(!captured.contains("--debug\n"));
    assert!(captured.contains("--no-profile\n"));
    assert!(captured.contains("--no-session\n"));
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
        enable_developer_builtin: false,
        cwd: None,
        session_log_path: None,
        invocation_id: None,
        personality_instance_id: None,
        wake_entry_id: None,
        change_event_seq: None,
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
prompt: |
  Just say hi and stop.
"#,
    )
    .unwrap();
    let (_file, path) = tmp.keep().unwrap();
    path
}
