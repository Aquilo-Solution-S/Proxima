//! M3 acceptance: a repo-scoped Code Fact fires a workspace wake, the
//! adapter runs in a disposable worktree, and `workspace-run-v1` plus
//! provenance edges land.

#![allow(clippy::too_many_lines)]

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use proxima_code::workspace_runner::CodeWorkspaceRunner;
use proxima_code::{
    CommitV1, EXECUTION_REQUEST_OBJECT_SCHEMA, EXECUTION_REQUEST_WHOLE_SCHEMA, ExecutionRequestV1,
    WorkspaceDecision, WorkspaceDecisionV1, WorkspaceReviewFinding, WorkspaceReviewV1,
    WorkspaceReviewVerdict,
};
use proxima_core::auth::NoAuth;
use proxima_core::harness::{ErrorClass, FinishReason};
use proxima_core::llm::{EmbeddingClient, LlmError};
use proxima_core::mcp::{McpAuthorContext, McpTool, McpToolCtx, OutputMode};
use proxima_core::personality::{
    InstantiatePersonalityRequest, SetWakeEntriesRequest, WakeEntryDraft, WakeExecutionMode,
};
use proxima_core::storage::Storage;
use proxima_core::verbs::event_ingest::{CitationMappingHint, CitedObjectHint, EventDraft};
use proxima_core::verbs::goal_write::{GoalAuthorshipKind, GoalState};
use proxima_core::verbs::query::MemoryStore;
use proxima_core::verbs::schema::{PayloadKind, SchemaInfo};
use proxima_core::wake::target_adapter::{
    TargetAdapter, TargetAdapterError, TargetContext, TargetInvocation, TargetOutcome,
    TargetOutcomeKind,
};
use proxima_core::{
    BindInferenceTierRequest, CORE_AUTHORED_RELATION, CORE_DERIVED_FROM_RELATION,
    CORE_WORKSPACE_RUN_OBJECT_SCHEMA, CORE_WORKSPACE_RUN_SOURCE_ID,
    CORE_WORKSPACE_RUN_WHOLE_SCHEMA, CoreWorkspaceDiffFile as WorkspaceDiffFile,
    CoreWorkspaceDiffStat as WorkspaceDiffStat, CoreWorkspaceRunV1, EdgeAuthorshipKind, Engine,
    EntityKind, FactPayload, FlavorRegistry, InferenceTargetConfig, MistralChatConfig, ModelTier,
    OrgId, Owner, OwnerPrincipalKind, Principal, RegisterInferenceTargetRequest, SchemaId,
    SchemaVersion, SourceBatchId, SourceId, UserId, WakeEntryAuthoredBy, WakeEntryTriggerKind,
    WorkspaceFinalizeInput, WorkspaceOutcome, WorkspacePrepareInput, WorkspaceRunner,
    WorkspaceRunnerError,
};
use proxima_flavor_goal::tools::mark_achieved::{
    MarkAchievedArgs, MarkAchievedStatus, MarkAchievedTool,
};
use proxima_flavor_goal::{GoalActivatedV1, SimpleTextGoalV1};
use proxima_storage_pg::PgStorage;
use proxima_storage_pg::verbs::edge_append::{EdgeDraft, append_edge_in_tx};
use proxima_storage_pg::verbs::event_ingest::ingest_event_in_tx;
use sqlx::{Connection, Executor, PgConnection, Row};
use tempfile::TempDir;
use uuid::Uuid;

const ADMIN_URL: &str = "postgres://proxima:proxima@localhost/postgres";
const EXECUTION_REQUEST_SOURCE_ID: &str = "proxima-code/execution-request";
const WORKSPACE_REVIEW_SOURCE_ID: &str = "proxima-code/workspace-review";
const WORKSPACE_REVIEW_OBJECT_SCHEMA: &str = "proxima-code/workspace-review-object-v1";
const WORKSPACE_REVIEW_WHOLE_SCHEMA: &str = "proxima-code/workspace-review-whole-v1";

#[derive(Debug, Clone, serde::Serialize)]
struct WorkspaceRunV1 {
    wake_invocation_id: Uuid,
    repo_id: Uuid,
    target_branch: String,
    worktree_path: String,
    branch_name: String,
    parent_sha: String,
    head_sha: String,
    diff_stat_json: WorkspaceDiffStat,
    exit_code: Option<i32>,
    stdout_tail: Option<String>,
    stderr_tail: Option<String>,
    duration_ms: Option<u64>,
}

impl WorkspaceRunV1 {
    const SCHEMA_ID: &'static str = CoreWorkspaceRunV1::SCHEMA_ID;

    fn core_payload(&self) -> CoreWorkspaceRunV1 {
        CoreWorkspaceRunV1 {
            wake_invocation_id: self.wake_invocation_id,
            wake_entry_id: Uuid::now_v7(),
            personality_instance_id: Uuid::now_v7(),
            binding_kind: "code_git_worktree".into(),
            finalize: "commit_all_candidate".into(),
            repo_path: self.worktree_path.clone(),
            base_ref: self.target_branch.clone(),
            worktree_path: self.worktree_path.clone(),
            branch_name: self.branch_name.clone(),
            parent_sha: self.parent_sha.clone(),
            head_sha: self.head_sha.clone(),
            committed: true,
            diff_stat_json: self.diff_stat_json.clone(),
            exit_code: self.exit_code,
            stdout_tail: self.stdout_tail.clone(),
            stderr_tail: self.stderr_tail.clone(),
            duration_ms: self.duration_ms,
            sandbox_image: None,
            sandbox_container: None,
            wake_branch: None,
            transcript_blob_hash: None,
            network_log_blob_hash: None,
        }
    }
}

#[derive(Debug)]
struct FakeEmbedding;

#[async_trait]
impl EmbeddingClient for FakeEmbedding {
    async fn embed(&self, _text: &str) -> Result<Vec<f32>, LlmError> {
        Ok(vec![0.0; 8])
    }

    fn model_id(&self) -> &'static str {
        "fake-embed"
    }

    fn dim(&self) -> usize {
        8
    }
}

#[derive(Debug, Clone)]
struct WorktreeWritingAdapter;

#[async_trait]
impl TargetAdapter for WorktreeWritingAdapter {
    async fn run(
        &self,
        invocation: TargetInvocation,
        _ctx: TargetContext,
    ) -> Result<TargetOutcome, TargetAdapterError> {
        let started = Instant::now();
        let Some(cwd) = invocation.workspace_root else {
            return Ok(target_failed(started, "missing cwd"));
        };
        let write_result = async {
            tokio::fs::write(cwd.join("workspace-output.txt"), b"workspace smoke\n")
                .await
                .map_err(|err| err.to_string())?;
            Ok::<(), String>(())
        }
        .await;
        match write_result {
            Ok(()) => Ok(TargetOutcome {
                kind: TargetOutcomeKind::Succeeded,
                finish_reason: FinishReason::Stop,
                error_class: ErrorClass::None,
                failure_reason: None,
                rounds_used: 1,
                duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                total_prompt_tokens: None,
                total_completion_tokens: None,
                tool_call_count: 0,
                jsonl_bytes: br#"{"record":"workspace-smoke"}"#.to_vec(),
                jsonl_truncated: false,
                network_log: None,
            }),
            Err(err) => Ok(target_failed(started, &err)),
        }
    }
}

fn target_failed(started: Instant, err: &str) -> TargetOutcome {
    TargetOutcome {
        kind: TargetOutcomeKind::Failed,
        finish_reason: FinishReason::Stop,
        error_class: ErrorClass::ToolDispatchFatal,
        failure_reason: Some(err.to_string()),
        rounds_used: 1,
        duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        total_prompt_tokens: None,
        total_completion_tokens: None,
        tool_call_count: 0,
        jsonl_bytes: format!(r#"{{"record":"workspace-smoke","error":{err:?}}}"#).into_bytes(),
        jsonl_truncated: false,
        network_log: None,
    }
}

async fn create_db(name: &str) -> Result<(), sqlx::Error> {
    let admin = std::env::var("PROXIMA_TEST_PG_URL").unwrap_or_else(|_| ADMIN_URL.into());
    let mut conn = PgConnection::connect(&admin).await?;
    conn.execute(format!("CREATE DATABASE \"{name}\"").as_str())
        .await?;
    conn.close().await?;
    Ok(())
}

async fn drop_db(name: &str) -> Result<(), sqlx::Error> {
    let admin = std::env::var("PROXIMA_TEST_PG_URL").unwrap_or_else(|_| ADMIN_URL.into());
    let mut conn = PgConnection::connect(&admin).await?;
    conn.execute(format!("DROP DATABASE IF EXISTS \"{name}\"").as_str())
        .await?;
    conn.close().await?;
    Ok(())
}

fn db_url(db_name: &str) -> String {
    let admin = std::env::var("PROXIMA_TEST_PG_URL").unwrap_or_else(|_| ADMIN_URL.into());
    match admin.rfind('/') {
        Some(idx) => format!("{}/{}", &admin[..idx], db_name),
        None => format!("{admin}/{db_name}"),
    }
}

async fn migrated_db() -> Option<(String, PgStorage)> {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let pg = PgStorage::connect(&db_url(&db_name))
        .await
        .expect("connect test db");
    if let Err(err) = async {
        pg.run_migrations().await?;
        proxima_code::migrator().run(pg.pool()).await?;
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await
    {
        drop(pg);
        let _ = drop_db(&db_name).await;
        panic!("migration failed: {err}");
    }
    Some((db_name, pg))
}

fn test_owner() -> Owner {
    Owner {
        principal: Principal::User(UserId::new(Uuid::now_v7())),
        org_id: OrgId::new(Uuid::now_v7()),
    }
}

fn git(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|err| err.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn init_repo(root: &TempDir) -> Result<(PathBuf, String), String> {
    let repo = root.path().join("repo");
    std::fs::create_dir(&repo).map_err(|err| err.to_string())?;
    git(&repo, &["init", "-b", "main"])?;
    std::fs::write(repo.join("README.md"), "initial\n").map_err(|err| err.to_string())?;
    git(&repo, &["add", "README.md"])?;
    git(
        &repo,
        &[
            "-c",
            "user.name=Proxima Test",
            "-c",
            "user.email=proxima@example.test",
            "commit",
            "-m",
            "initial",
        ],
    )?;
    let head = git(&repo, &["rev-parse", "HEAD"])?;
    Ok((repo, head))
}

#[cfg(unix)]
fn fake_pnpm(root: &TempDir) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let path = root.path().join("pnpm");
    std::fs::write(
        &path,
        r#"#!/bin/sh
printf '%s
' "$@" > "$PWD/pnpm-args.txt"
mkdir -p "$PWD/node_modules" "$PWD/packages/frontend-core/node_modules"
printf hydrated > "$PWD/node_modules/.proxima-pnpm-hydrated"
"#,
    )?;
    let mut permissions = std::fs::metadata(&path)?.permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&path, permissions)?;
    Ok(path)
}

fn registry_with_runner(
    pg: &PgStorage,
    worktrees_root: PathBuf,
) -> proxima_core::FlavorRegistryFrozen {
    let mut registry = FlavorRegistry::new();
    proxima_code::register(&mut registry);
    registry.replace_workspace_runner(
        "proxima-code",
        Arc::new(CodeWorkspaceRunner::new(pg.pool().clone()).with_worktrees_root(worktrees_root)),
    );
    registry.freeze().with_additional_schemas([
        SchemaInfo::opaque(
            SchemaId::new(proxima_code::CODE_BLOB_SCHEMA.into()),
            SchemaVersion::new(1),
            PayloadKind::CitedObject,
        ),
        SchemaInfo::opaque(
            SchemaId::new(proxima_code::CODE_COMMIT_OBJECT_SCHEMA.into()),
            SchemaVersion::new(1),
            PayloadKind::CitedObject,
        ),
        SchemaInfo::opaque(
            SchemaId::new(CORE_WORKSPACE_RUN_OBJECT_SCHEMA.into()),
            SchemaVersion::new(1),
            PayloadKind::CitedObject,
        ),
        SchemaInfo::opaque(
            SchemaId::new(proxima_code::CODE_COMMIT_WHOLE_SCHEMA.into()),
            SchemaVersion::new(1),
            PayloadKind::CitationMapping,
        ),
        SchemaInfo::opaque(
            SchemaId::new(CORE_WORKSPACE_RUN_WHOLE_SCHEMA.into()),
            SchemaVersion::new(1),
            PayloadKind::CitationMapping,
        ),
    ])
}

async fn seed_execution_request_for_runner(
    pg: &PgStorage,
    owner: &Owner,
    repo_id: Uuid,
    request_key: &str,
    title: &str,
    instructions: &str,
) -> Result<proxima_core::MemoryId, Box<dyn std::error::Error>> {
    let payload = ExecutionRequestV1 {
        repo_id,
        title: title.into(),
        instructions: instructions.into(),
        request_key: request_key.into(),
    };
    let mut payload_bytes = Vec::new();
    ciborium::ser::into_writer(&payload, &mut payload_bytes)?;
    let content_hash = blake3::hash(&payload_bytes);
    let observed_at = time::OffsetDateTime::now_utc();
    let draft = EventDraft {
        source_id: SourceId::new(EXECUTION_REQUEST_SOURCE_ID),
        source_batch_id: SourceBatchId::new(Uuid::now_v7()),
        owner: owner.clone(),
        schema_id: SchemaId::new(ExecutionRequestV1::SCHEMA_ID.into()),
        schema_version: SchemaVersion::new(ExecutionRequestV1::SCHEMA_VERSION),
        payload: payload_bytes,
        observed_at,
        occurred_at: observed_at,
        cited_object: CitedObjectHint {
            schema_id: SchemaId::new(EXECUTION_REQUEST_OBJECT_SCHEMA.into()),
            schema_version: SchemaVersion::new(1),
            content_hash: *content_hash.as_bytes(),
        },
        citation_mapping: CitationMappingHint {
            schema_id: SchemaId::new(EXECUTION_REQUEST_WHOLE_SCHEMA.into()),
            schema_version: SchemaVersion::new(1),
        },
    };
    let mut tx = pg.pool().begin().await?;
    let outcome = ingest_event_in_tx(&mut tx, &draft).await?;
    sqlx::query(
        "INSERT INTO proxima_code.execution_request_v1
            (memory_id, repo_id, title, instructions, request_key)
         VALUES ($1,$2,$3,$4,$5)",
    )
    .bind(outcome.memory_id.into_inner())
    .bind(repo_id)
    .bind(title)
    .bind(instructions)
    .bind(request_key)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(outcome.memory_id)
}

async fn seed_workspace_run_for_runner(
    pg: &PgStorage,
    owner: &Owner,
    registry: &proxima_core::FlavorRegistryFrozen,
    payload: &WorkspaceRunV1,
    request: proxima_core::MemoryId,
) -> Result<proxima_core::MemoryId, Box<dyn std::error::Error>> {
    let core_payload = payload.core_payload();
    let mut payload_bytes = Vec::new();
    ciborium::ser::into_writer(&core_payload, &mut payload_bytes)?;
    let content_hash = blake3::hash(&payload_bytes);
    let observed_at = time::OffsetDateTime::now_utc();
    let draft = EventDraft {
        source_id: SourceId::new(CORE_WORKSPACE_RUN_SOURCE_ID),
        source_batch_id: SourceBatchId::new(Uuid::now_v7()),
        owner: owner.clone(),
        schema_id: SchemaId::new(CoreWorkspaceRunV1::SCHEMA_ID.into()),
        schema_version: SchemaVersion::new(CoreWorkspaceRunV1::SCHEMA_VERSION),
        payload: payload_bytes,
        observed_at,
        occurred_at: observed_at,
        cited_object: CitedObjectHint {
            schema_id: SchemaId::new(CORE_WORKSPACE_RUN_OBJECT_SCHEMA.into()),
            schema_version: SchemaVersion::new(1),
            content_hash: *content_hash.as_bytes(),
        },
        citation_mapping: CitationMappingHint {
            schema_id: SchemaId::new(CORE_WORKSPACE_RUN_WHOLE_SCHEMA.into()),
            schema_version: SchemaVersion::new(1),
        },
    };
    let mut tx = pg.pool().begin().await?;
    let outcome = ingest_event_in_tx(&mut tx, &draft).await?;
    sqlx::query(
        "INSERT INTO proxima_core.workspace_run_v1
            (memory_id, wake_invocation_id, wake_entry_id, personality_instance_id,
             binding_kind, finalize, repo_path, base_ref, worktree_path,
             branch_name, parent_sha, head_sha, committed, diff_stat_json, exit_code,
             stdout_tail, stderr_tail, duration_ms)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18)",
    )
    .bind(outcome.memory_id.into_inner())
    .bind(core_payload.wake_invocation_id)
    .bind(core_payload.wake_entry_id)
    .bind(core_payload.personality_instance_id)
    .bind(&core_payload.binding_kind)
    .bind(&core_payload.finalize)
    .bind(&core_payload.repo_path)
    .bind(&core_payload.base_ref)
    .bind(&core_payload.worktree_path)
    .bind(&core_payload.branch_name)
    .bind(&core_payload.parent_sha)
    .bind(&core_payload.head_sha)
    .bind(core_payload.committed)
    .bind(serde_json::to_value(&core_payload.diff_stat_json)?)
    .bind(core_payload.exit_code)
    .bind(core_payload.stdout_tail.as_deref())
    .bind(core_payload.stderr_tail.as_deref())
    .bind(
        core_payload
            .duration_ms
            .and_then(|value| i64::try_from(value).ok()),
    )
    .execute(&mut *tx)
    .await?;
    let relation = registry
        .resolve_relation(CORE_DERIVED_FROM_RELATION)
        .expect("core/derived-from registered");
    append_edge_in_tx(
        &mut tx,
        &EdgeDraft {
            edge_id: Uuid::now_v7(),
            relation,
            source_kind: EntityKind::Fact,
            source_memory_id: Some(outcome.memory_id.into_inner()),
            source_goal_id: None,
            target_kind: EntityKind::Fact,
            target_memory_id: Some(request.into_inner()),
            target_goal_id: None,
            authorship_kind: EdgeAuthorshipKind::EventSource,
            authorship_owner_memory_id: None,
            owner,
        },
        None,
    )
    .await?;
    tx.commit().await?;
    Ok(outcome.memory_id)
}

async fn seed_workspace_review_for_runner(
    pg: &PgStorage,
    owner: &Owner,
    payload: &WorkspaceReviewV1,
) -> Result<proxima_core::MemoryId, Box<dyn std::error::Error>> {
    let mut payload_bytes = Vec::new();
    ciborium::ser::into_writer(payload, &mut payload_bytes)?;
    let content_hash = blake3::hash(&payload_bytes);
    let draft = EventDraft {
        source_id: SourceId::new(WORKSPACE_REVIEW_SOURCE_ID),
        source_batch_id: SourceBatchId::new(Uuid::now_v7()),
        owner: owner.clone(),
        schema_id: SchemaId::new(WorkspaceReviewV1::SCHEMA_ID.into()),
        schema_version: SchemaVersion::new(WorkspaceReviewV1::SCHEMA_VERSION),
        payload: payload_bytes,
        observed_at: time::OffsetDateTime::now_utc(),
        occurred_at: payload.reviewed_at,
        cited_object: CitedObjectHint {
            schema_id: SchemaId::new(WORKSPACE_REVIEW_OBJECT_SCHEMA.into()),
            schema_version: SchemaVersion::new(1),
            content_hash: *content_hash.as_bytes(),
        },
        citation_mapping: CitationMappingHint {
            schema_id: SchemaId::new(WORKSPACE_REVIEW_WHOLE_SCHEMA.into()),
            schema_version: SchemaVersion::new(1),
        },
    };
    let mut tx = pg.pool().begin().await?;
    let outcome = ingest_event_in_tx(&mut tx, &draft).await?;
    sqlx::query(
        "INSERT INTO proxima_code.workspace_review_v1
            (memory_id, workspace_run_memory_id, execution_request_memory_id,
             verdict, round_index, summary, findings_json,
             correction_instructions, verification_summary, reviewed_at)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
    )
    .bind(outcome.memory_id.into_inner())
    .bind(payload.workspace_run_memory_id)
    .bind(payload.execution_request_memory_id)
    .bind(payload.verdict)
    .bind(i32::try_from(payload.round_index).unwrap_or(i32::MAX))
    .bind(&payload.summary)
    .bind(serde_json::to_value(&payload.findings)?)
    .bind(payload.correction_instructions.as_deref())
    .bind(payload.verification_summary.as_deref())
    .bind(payload.reviewed_at)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(outcome.memory_id)
}

async fn append_derived_edge_for_runner(
    pg: &PgStorage,
    owner: &Owner,
    registry: &proxima_core::FlavorRegistryFrozen,
    source: proxima_core::MemoryId,
    target: proxima_core::MemoryId,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut tx = pg.pool().begin().await?;
    let relation = registry
        .resolve_relation(CORE_DERIVED_FROM_RELATION)
        .expect("core/derived-from registered");
    append_edge_in_tx(
        &mut tx,
        &EdgeDraft {
            edge_id: Uuid::now_v7(),
            relation,
            source_kind: EntityKind::Fact,
            source_memory_id: Some(source.into_inner()),
            source_goal_id: None,
            target_kind: EntityKind::Fact,
            target_memory_id: Some(target.into_inner()),
            target_goal_id: None,
            authorship_kind: EdgeAuthorshipKind::EventSource,
            authorship_owner_memory_id: None,
            owner,
        },
        None,
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn approved_execution_request_suppresses_duplicate_workspace_prepare()
-> Result<(), Box<dyn std::error::Error>> {
    let Some((db_name, pg)) = migrated_db().await else {
        return Ok(());
    };
    let result = async {
        let owner = test_owner();
        let repo_id = Uuid::now_v7();
        let registry_root = tempfile::tempdir()?;
        let registry = registry_with_runner(&pg, registry_root.path().to_path_buf());
        let request = seed_execution_request_for_runner(
            &pg,
            &owner,
            repo_id,
            "terminal-request",
            "Terminal request",
            "Make the requested change.",
        )
        .await?;
        let run_payload = WorkspaceRunV1 {
            wake_invocation_id: Uuid::now_v7(),
            repo_id,
            target_branch: "main".into(),
            worktree_path: "/tmp/proxima-terminal-request".into(),
            branch_name: "proxima/wake/terminal".into(),
            parent_sha: "0".repeat(40),
            head_sha: "1".repeat(40),
            diff_stat_json: WorkspaceDiffStat {
                files_changed: 1,
                insertions: 1,
                deletions: 0,
                files: vec![WorkspaceDiffFile {
                    path: "README.md".into(),
                    insertions: 1,
                    deletions: 0,
                }],
            },
            exit_code: Some(0),
            stdout_tail: None,
            stderr_tail: None,
            duration_ms: Some(100),
        };
        let run =
            seed_workspace_run_for_runner(&pg, &owner, &registry, &run_payload, request).await?;
        let review_payload = WorkspaceReviewV1 {
            workspace_run_memory_id: run.into_inner(),
            execution_request_memory_id: request.into_inner(),
            verdict: WorkspaceReviewVerdict::Approved,
            round_index: 0,
            summary: "Approved.".into(),
            findings: Vec::new(),
            correction_instructions: None,
            verification_summary: Some("checks passed".into()),
            reviewed_at: time::OffsetDateTime::now_utc(),
        };
        seed_workspace_review_for_runner(&pg, &owner, &review_payload).await?;

        let payload = serde_json::to_value(ExecutionRequestV1 {
            repo_id,
            title: "Terminal request".into(),
            instructions: "Make the requested change.".into(),
            request_key: "terminal-request".into(),
        })?;
        let runner = CodeWorkspaceRunner::new(pg.pool().clone());
        let err = match runner
            .prepare(WorkspacePrepareInput {
                invocation_id: Uuid::now_v7(),
                owner: &owner,
                wake_token: Uuid::now_v7(),
                mcp_url: "http://127.0.0.1:1/mcp",
                root_perspective_memory_id: proxima_core::MemoryId::new(Uuid::now_v7()),
                triggering_memory_id: request,
                triggering_memory_schema_id: ExecutionRequestV1::SCHEMA_ID,
                triggering_memory_payload: &payload,
                is_continuation: false,
                workspace_tool_palette: &[],
            })
            .await
        {
            Ok(_) => panic!("terminal request should not prepare another workspace run"),
            Err(err) => err,
        };

        match err {
            WorkspaceRunnerError::TriggerNotEligible(reason) => {
                assert!(reason.contains("terminal workspace review"), "{reason}");
            }
            other => panic!("unexpected error: {other}"),
        }
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db_name).await;
    result
}

async fn seed_goal_context_for_runner(
    pg: &PgStorage,
    owner: &Owner,
    request: proxima_core::MemoryId,
    registry: &proxima_core::FlavorRegistryFrozen,
) -> Result<(Uuid, proxima_core::MemoryId), Box<dyn std::error::Error>> {
    let goal_id = Uuid::now_v7();
    let mut goal_payload = Vec::new();
    ciborium::ser::into_writer(&SimpleTextGoalV1 {}, &mut goal_payload)?;
    let owner_kind = OwnerPrincipalKind::of(&owner.principal);
    let owner_principal_id = match &owner.principal {
        Principal::User(user) => user.into_inner(),
        Principal::Group(group) => group.into_inner(),
    };
    let owner_org_id = owner.org_id.into_inner();
    sqlx::query(
        "INSERT INTO proxima_core.goals
            (goal_id, schema_id, schema_version, owner_principal_kind,
             owner_principal_id, owner_org_id, title, text, payload, state,
             authorship_kind, request_id)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)",
    )
    .bind(goal_id)
    .bind("proxima-goal/simple-text-v1")
    .bind(1_i32)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind("Ship closure")
    .bind("Acceptance criteria: close the verifier loop.")
    .bind(goal_payload)
    .bind(GoalState::Active)
    .bind(GoalAuthorshipKind::User)
    .bind(format!("goal-{}", Uuid::now_v7()))
    .execute(pg.pool())
    .await?;

    let payload = GoalActivatedV1 {
        goal_id,
        schema_id: "proxima-goal/simple-text-v1".into(),
        title: "Ship closure".into(),
        accepted_at: time::OffsetDateTime::now_utc(),
        evidence_count: 0,
    };
    let mut payload_bytes = Vec::new();
    ciborium::ser::into_writer(&payload, &mut payload_bytes)?;
    let content_hash = blake3::hash(&payload_bytes);
    let draft = EventDraft {
        source_id: SourceId::new("proxima-goal/goal-activated"),
        source_batch_id: SourceBatchId::new(Uuid::now_v7()),
        owner: owner.clone(),
        schema_id: SchemaId::new(GoalActivatedV1::SCHEMA_ID.into()),
        schema_version: SchemaVersion::new(GoalActivatedV1::SCHEMA_VERSION),
        payload: payload_bytes,
        observed_at: time::OffsetDateTime::now_utc(),
        occurred_at: payload.accepted_at,
        cited_object: CitedObjectHint {
            schema_id: SchemaId::new("proxima-goal/goal-activated-object-v1".into()),
            schema_version: SchemaVersion::new(1),
            content_hash: *content_hash.as_bytes(),
        },
        citation_mapping: CitationMappingHint {
            schema_id: SchemaId::new("proxima-goal/goal-activated-whole-v1".into()),
            schema_version: SchemaVersion::new(1),
        },
    };
    let mut tx = pg.pool().begin().await?;
    let outcome = ingest_event_in_tx(&mut tx, &draft).await?;
    sqlx::query(
        "INSERT INTO proxima_goal.goal_activated_v1
            (memory_id, goal_id, schema_id, title, accepted_at, evidence_count)
         VALUES ($1,$2,$3,$4,$5,$6)",
    )
    .bind(outcome.memory_id.into_inner())
    .bind(goal_id)
    .bind(&payload.schema_id)
    .bind(&payload.title)
    .bind(payload.accepted_at)
    .bind(i32::try_from(payload.evidence_count).unwrap_or(i32::MAX))
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    append_derived_edge_for_runner(pg, owner, registry, request, outcome.memory_id).await?;
    Ok((goal_id, outcome.memory_id))
}

#[tokio::test(flavor = "multi_thread")]
async fn code_workspace_prepare_builds_context_with_preloaded_paths()
-> Result<(), Box<dyn std::error::Error>> {
    let Some((db_name, pg)) = migrated_db().await else {
        return Ok(());
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = test_owner();
        let repo_root = tempfile::tempdir()?;
        let worktrees_root = tempfile::tempdir()?;
        let (repo_path, _) = init_repo(&repo_root).map_err(std::io::Error::other)?;
        std::fs::create_dir(repo_path.join("src"))?;
        std::fs::write(repo_path.join("src/lib.rs"), "pub fn target() {}\n")?;
        std::fs::write(repo_path.join("large.txt"), "x".repeat(70 * 1024))?;
        git(&repo_path, &["add", "src/lib.rs", "large.txt"])?;
        git(
            &repo_path,
            &[
                "-c",
                "user.name=Proxima Test",
                "-c",
                "user.email=proxima@example.test",
                "commit",
                "-m",
                "add target files",
            ],
        )?;
        let parent_sha = git(&repo_path, &["rev-parse", "main"])?;
        let repo_id = Uuid::now_v7();
        proxima_code::register_repo(
            pg.pool(),
            &owner,
            repo_id,
            repo_path.to_str().expect("utf8 repo path"),
            "workspace-context",
        )
        .await?;

        let payload = serde_json::to_value(ExecutionRequestV1 {
            repo_id,
            title: "Update target files".into(),
            instructions: "Touch `README.md`, `src/lib.rs`, and `large.txt`.".into(),
            request_key: "workspace-context-request".into(),
        })?;
        let runner = CodeWorkspaceRunner::new(pg.pool().clone())
            .with_worktrees_root(worktrees_root.path().to_path_buf());
        let prepared = runner
            .prepare(WorkspacePrepareInput {
                invocation_id: Uuid::now_v7(),
                owner: &owner,
                wake_token: Uuid::now_v7(),
                mcp_url: "http://127.0.0.1:1/mcp",
                root_perspective_memory_id: proxima_core::MemoryId::new(Uuid::now_v7()),
                triggering_memory_id: proxima_core::MemoryId::new(Uuid::now_v7()),
                triggering_memory_schema_id: ExecutionRequestV1::SCHEMA_ID,
                triggering_memory_payload: &payload,
                is_continuation: false,
                workspace_tool_palette: &[],
            })
            .await?;

        let context = prepared.workspace_context.expect("workspace context");
        assert_eq!(context["repo_id"], repo_id.to_string());
        assert_eq!(context["parent_sha"], parent_sha);
        assert_eq!(context["request_key"], "workspace-context-request");
        assert_eq!(
            context["worktree_path"].as_str().expect("worktree path"),
            prepared.work_dir.to_string_lossy()
        );
        assert_eq!(context["tooling"]["frontend"]["pnpm"]["status"], "skipped");
        assert_eq!(
            context["tooling"]["frontend"]["pnpm"]["reason"],
            "no_pnpm_lock"
        );
        let files = context["preloaded_files"]["files"]
            .as_array()
            .expect("preloaded files");
        assert_eq!(context["preloaded_files"]["limits"]["max_files"], 3);
        assert_eq!(
            context["preloaded_files"]["limits"]["max_file_bytes"],
            24 * 1024
        );
        assert_eq!(
            context["preloaded_files"]["limits"]["max_total_bytes"],
            48 * 1024
        );
        assert!(
            files.iter().any(|file| file["path"] == "README.md"
                && file["content"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("initial")),
            "README.md should be preloaded"
        );
        assert!(
            files.iter().any(|file| file["path"] == "README.md"
                && file["line_count"] == 1
                && file["sha256"].as_str().is_some_and(|hash| hash.len() == 64)),
            "preloaded files should carry compact metadata"
        );
        assert!(
            files.iter().any(|file| file["path"] == "src/lib.rs"
                && file["content"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("target")),
            "src/lib.rs should be preloaded"
        );
        let omitted = context["preloaded_files"]["omitted"]
            .as_array()
            .expect("omitted files");
        assert!(
            omitted
                .iter()
                .any(|file| file["path"] == "large.txt" && file["reason"] == "file_too_large"),
            "large mentioned files should be listed as omitted"
        );
        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test(flavor = "multi_thread")]
async fn emit_workspace_decision_writes_typed_decides_edge()
-> Result<(), Box<dyn std::error::Error>> {
    let Some((db_name, pg)) = migrated_db().await else {
        return Ok(());
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = test_owner();
        let mut registry = FlavorRegistry::new();
        proxima_code::register(&mut registry);
        let registry = Arc::new(registry.freeze());
        let repo_id = Uuid::now_v7();
        let request = seed_execution_request_for_runner(
            &pg,
            &owner,
            repo_id,
            "decides-request",
            "Decides request",
            "Make the change.",
        )
        .await?;
        let run = seed_workspace_run_for_runner(
            &pg,
            &owner,
            &registry,
            &WorkspaceRunV1 {
                wake_invocation_id: Uuid::now_v7(),
                repo_id,
                target_branch: "main".into(),
                worktree_path: "/tmp/proxima-decision-test".into(),
                branch_name: format!("proxima/wake/{}", Uuid::now_v7()),
                parent_sha: "0000000".into(),
                head_sha: "1111111".into(),
                diff_stat_json: WorkspaceDiffStat {
                    files_changed: 1,
                    insertions: 3,
                    deletions: 1,
                    files: Vec::new(),
                },
                exit_code: Some(0),
                stdout_tail: Some("ok".into()),
                stderr_tail: None,
                duration_ms: Some(42),
            },
            request,
        )
        .await?;

        let decision_memory = proxima_code::emit_workspace_decision(
            pg.pool(),
            &owner,
            run,
            WorkspaceDecision::Rejected,
            Some("test reject"),
        )
        .await?;

        let decides_edges: i64 = sqlx::query_scalar(
            "SELECT count(*)
             FROM proxima_core.edges
             WHERE relation = $1
               AND source_kind = 'Fact'
               AND source_memory_id = $2
               AND target_kind = 'Fact'
               AND target_memory_id = $3",
        )
        .bind(proxima_code::CODE_DECIDES_RELATION)
        .bind(decision_memory.into_inner())
        .bind(run.into_inner())
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(decides_edges, 1, "missing proxima-code/decides edge");

        let stale_derived: i64 = sqlx::query_scalar(
            "SELECT count(*)
             FROM proxima_core.edges
             WHERE relation = $1
               AND source_kind = 'Fact'
               AND source_memory_id = $2
               AND target_kind = 'Fact'
               AND target_memory_id = $3",
        )
        .bind(CORE_DERIVED_FROM_RELATION)
        .bind(decision_memory.into_inner())
        .bind(run.into_inner())
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(
            stale_derived, 0,
            "decision→run derived edge must be replaced"
        );

        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db_name).await;
    result
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn code_workspace_runner_hydrates_pnpm_tooling() -> Result<(), Box<dyn std::error::Error>> {
    let Some((db_name, pg)) = migrated_db().await else {
        return Ok(());
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = test_owner();
        let repo_root = tempfile::tempdir()?;
        let worktrees_root = tempfile::tempdir()?;
        let pnpm_store_root = tempfile::tempdir()?;
        let fake_pnpm_root = tempfile::tempdir()?;
        let (repo_path, _) = init_repo(&repo_root).map_err(std::io::Error::other)?;
        std::fs::write(repo_path.join("pnpm-lock.yaml"), "lockfileVersion: '9.0'\n")?;
        std::fs::write(repo_path.join("package.json"), "{\"private\":true}\n")?;
        std::fs::create_dir_all(repo_path.join("packages/frontend-core"))?;
        std::fs::write(
            repo_path.join("packages/frontend-core/package.json"),
            "{\"name\":\"@proxima/core\"}\n",
        )?;
        git(
            &repo_path,
            &[
                "add",
                "pnpm-lock.yaml",
                "package.json",
                "packages/frontend-core/package.json",
            ],
        )?;
        git(
            &repo_path,
            &[
                "-c",
                "user.name=Proxima Test",
                "-c",
                "user.email=proxima@example.test",
                "commit",
                "-m",
                "add pnpm workspace",
            ],
        )?;
        let repo_id = Uuid::now_v7();
        proxima_code::register_repo(
            pg.pool(),
            &owner,
            repo_id,
            repo_path.to_str().expect("utf8 repo path"),
            "pnpm-tooling",
        )
        .await?;

        let payload = serde_json::to_value(ExecutionRequestV1 {
            repo_id,
            title: "Verify frontend".into(),
            instructions: "Run `pnpm --filter @proxima/core typecheck`.".into(),
            request_key: "pnpm-tooling-request".into(),
        })?;
        let pnpm_executable = fake_pnpm(&fake_pnpm_root)?;
        let runner = CodeWorkspaceRunner::new(pg.pool().clone())
            .with_worktrees_root(worktrees_root.path().to_path_buf())
            .with_pnpm_store_root(pnpm_store_root.path().to_path_buf())
            .with_pnpm_executable(pnpm_executable);
        let prepared = runner
            .prepare(WorkspacePrepareInput {
                invocation_id: Uuid::now_v7(),
                owner: &owner,
                wake_token: Uuid::now_v7(),
                mcp_url: "http://127.0.0.1:1/mcp",
                root_perspective_memory_id: proxima_core::MemoryId::new(Uuid::now_v7()),
                triggering_memory_id: proxima_core::MemoryId::new(Uuid::now_v7()),
                triggering_memory_schema_id: ExecutionRequestV1::SCHEMA_ID,
                triggering_memory_payload: &payload,
                is_continuation: false,
                workspace_tool_palette: &[],
            })
            .await?;

        let context = prepared.workspace_context.expect("workspace context");
        let pnpm = &context["tooling"]["frontend"]["pnpm"];
        assert_eq!(pnpm["status"], "succeeded");
        assert_eq!(pnpm["exit_code"], 0);
        assert_eq!(
            pnpm["store_dir"].as_str().expect("store dir"),
            pnpm_store_root.path().to_string_lossy()
        );
        assert!(
            prepared
                .work_dir
                .join("node_modules/.proxima-pnpm-hydrated")
                .exists(),
            "pnpm hydration should create worktree-local node_modules"
        );
        let args = std::fs::read_to_string(prepared.work_dir.join("pnpm-args.txt"))?;
        assert!(args.contains("install"));
        assert!(args.contains("--frozen-lockfile"));
        assert!(args.contains("--prefer-offline"));
        assert!(args.contains("--store-dir"));
        assert!(args.contains(&pnpm_store_root.path().to_string_lossy().to_string()));
        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test(flavor = "multi_thread")]
async fn workspace_run_trigger_prepares_verifier_context_from_worker_branch()
-> Result<(), Box<dyn std::error::Error>> {
    let Some((db_name, pg)) = migrated_db().await else {
        return Ok(());
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = test_owner();
        let repo_root = tempfile::tempdir()?;
        let worktree_root = tempfile::tempdir()?;
        let (repo_path, parent_sha) = init_repo(&repo_root).map_err(std::io::Error::other)?;
        let branch_name = format!("proxima/wake/{}", Uuid::now_v7());
        let worker_tree = worktree_root.path().join("worker");
        git(
            &repo_path,
            &[
                "worktree",
                "add",
                "-b",
                &branch_name,
                worker_tree.to_str().expect("utf8 worktree"),
                "main",
            ],
        )
        .map_err(std::io::Error::other)?;
        std::fs::write(worker_tree.join("README.md"), "initial\nverified change\n")?;
        git(&worker_tree, &["add", "README.md"]).map_err(std::io::Error::other)?;
        git(
            &worker_tree,
            &[
                "-c",
                "user.name=Proxima Test",
                "-c",
                "user.email=proxima@example.test",
                "commit",
                "-m",
                "worker change",
            ],
        )
        .map_err(std::io::Error::other)?;
        let head_sha = git(&worker_tree, &["rev-parse", "HEAD"]).map_err(std::io::Error::other)?;

        let repo_id = Uuid::now_v7();
        proxima_code::register_repo(
            pg.pool(),
            &owner,
            repo_id,
            repo_path.to_str().expect("utf8 repo path"),
            "review-context",
        )
        .await?;
        let registry_root = tempfile::tempdir()?;
        let registry = registry_with_runner(&pg, registry_root.path().to_path_buf());
        let request = seed_execution_request_for_runner(
            &pg,
            &owner,
            repo_id,
            "review-context-request",
            "Update README",
            "Update `README.md`.",
        )
        .await?;
        let run_payload = WorkspaceRunV1 {
            wake_invocation_id: Uuid::now_v7(),
            repo_id,
            target_branch: "main".into(),
            worktree_path: worker_tree.to_string_lossy().to_string(),
            branch_name: branch_name.clone(),
            parent_sha: parent_sha.clone(),
            head_sha: head_sha.clone(),
            diff_stat_json: WorkspaceDiffStat {
                files_changed: 1,
                insertions: 1,
                deletions: 0,
                files: vec![WorkspaceDiffFile {
                    path: "README.md".into(),
                    insertions: 1,
                    deletions: 0,
                }],
            },
            exit_code: Some(0),
            stdout_tail: Some("worker stdout".into()),
            stderr_tail: Some("worker stderr".into()),
            duration_ms: Some(123),
        };
        let run =
            seed_workspace_run_for_runner(&pg, &owner, &registry, &run_payload, request).await?;
        let payload = serde_json::to_value(&run_payload)?;
        let runner = CodeWorkspaceRunner::new(pg.pool().clone());
        let prepared = runner
            .prepare(WorkspacePrepareInput {
                invocation_id: Uuid::now_v7(),
                owner: &owner,
                wake_token: Uuid::now_v7(),
                mcp_url: "http://127.0.0.1:1/mcp",
                root_perspective_memory_id: proxima_core::MemoryId::new(Uuid::now_v7()),
                triggering_memory_id: run,
                triggering_memory_schema_id: WorkspaceRunV1::SCHEMA_ID,
                triggering_memory_payload: &payload,
                is_continuation: false,
                workspace_tool_palette: &[],
            })
            .await?;

        assert_eq!(prepared.work_dir, worker_tree);
        let context = prepared.workspace_context.expect("workspace context");
        assert_eq!(context["mode"], "verify_workspace_run");
        assert_eq!(context["repo_id"], repo_id.to_string());
        assert_eq!(
            context["workspace_run_memory_id"],
            run.into_inner().to_string()
        );
        assert_eq!(
            context["original_request"]["payload"]["request_key"],
            "review-context-request"
        );
        assert_eq!(context["diff"]["name_only"][0], "README.md");
        assert!(
            context["diff"]["patch"]
                .as_str()
                .unwrap_or_default()
                .contains("verified change")
        );
        assert_eq!(context["diff"]["patch_truncated"], false);
        assert_eq!(context["log_tails"]["stdout_tail"], "worker stdout");
        assert_eq!(context["log_tails"]["stderr_tail"], "worker stderr");
        assert_eq!(context["veto_count"], 0);
        assert_eq!(
            git(&prepared.work_dir, &["rev-parse", "HEAD"]).map_err(std::io::Error::other)?,
            head_sha
        );
        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test(flavor = "multi_thread")]
async fn workspace_run_review_finalize_does_not_emit_workspace_run()
-> Result<(), Box<dyn std::error::Error>> {
    let Some((db_name, pg)) = migrated_db().await else {
        return Ok(());
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = test_owner();
        let repo_root = tempfile::tempdir()?;
        let worktree_root = tempfile::tempdir()?;
        let (repo_path, parent_sha) = init_repo(&repo_root).map_err(std::io::Error::other)?;
        let branch_name = format!("proxima/wake/{}", Uuid::now_v7());
        let worker_tree = worktree_root.path().join("worker");
        git(
            &repo_path,
            &[
                "worktree",
                "add",
                "-b",
                &branch_name,
                worker_tree.to_str().expect("utf8 worktree"),
                "main",
            ],
        )
        .map_err(std::io::Error::other)?;
        std::fs::write(worker_tree.join("README.md"), "initial\nverified change\n")?;
        git(&worker_tree, &["add", "README.md"]).map_err(std::io::Error::other)?;
        git(
            &worker_tree,
            &[
                "-c",
                "user.name=Proxima Test",
                "-c",
                "user.email=proxima@example.test",
                "commit",
                "-m",
                "worker change",
            ],
        )
        .map_err(std::io::Error::other)?;
        let head_sha = git(&worker_tree, &["rev-parse", "HEAD"]).map_err(std::io::Error::other)?;

        let repo_id = Uuid::now_v7();
        proxima_code::register_repo(
            pg.pool(),
            &owner,
            repo_id,
            repo_path.to_str().expect("utf8 repo path"),
            "inspect-only-finalize",
        )
        .await?;
        let registry_root = tempfile::tempdir()?;
        let registry = registry_with_runner(&pg, registry_root.path().to_path_buf());
        let request = seed_execution_request_for_runner(
            &pg,
            &owner,
            repo_id,
            "inspect-only-finalize-request",
            "Update README",
            "Update `README.md`.",
        )
        .await?;
        let run_payload = WorkspaceRunV1 {
            wake_invocation_id: Uuid::now_v7(),
            repo_id,
            target_branch: "main".into(),
            worktree_path: worker_tree.to_string_lossy().to_string(),
            branch_name,
            parent_sha: parent_sha.clone(),
            head_sha,
            diff_stat_json: WorkspaceDiffStat {
                files_changed: 1,
                insertions: 1,
                deletions: 0,
                files: vec![WorkspaceDiffFile {
                    path: "README.md".into(),
                    insertions: 1,
                    deletions: 0,
                }],
            },
            exit_code: Some(0),
            stdout_tail: Some("worker stdout".into()),
            stderr_tail: None,
            duration_ms: Some(123),
        };
        let run =
            seed_workspace_run_for_runner(&pg, &owner, &registry, &run_payload, request).await?;
        let runner = CodeWorkspaceRunner::new(pg.pool().clone());
        let prepared = runner
            .prepare(WorkspacePrepareInput {
                invocation_id: Uuid::now_v7(),
                owner: &owner,
                wake_token: Uuid::now_v7(),
                mcp_url: "http://127.0.0.1:1/mcp",
                root_perspective_memory_id: proxima_core::MemoryId::new(Uuid::now_v7()),
                triggering_memory_id: run,
                triggering_memory_schema_id: WorkspaceRunV1::SCHEMA_ID,
                triggering_memory_payload: &serde_json::to_value(&run_payload)?,
                is_continuation: false,
                workspace_tool_palette: &[],
            })
            .await?;
        let before: i64 = sqlx::query_scalar("SELECT count(*) FROM proxima_core.workspace_run_v1")
            .fetch_one(pg.pool())
            .await?;
        let record = runner
            .finalize(WorkspaceFinalizeInput {
                owner: &owner,
                invocation_id: Uuid::now_v7(),
                wake_entry_id: Uuid::now_v7(),
                personality_instance_id: proxima_core::PersonalityInstanceId::new(Uuid::now_v7()),
                root_perspective_memory_id: proxima_core::MemoryId::new(Uuid::now_v7()),
                triggering_memory_id: run,
                authored_relation: registry
                    .resolve_relation(CORE_AUTHORED_RELATION)
                    .expect("core/authored registered"),
                derived_from_relation: registry
                    .resolve_relation(CORE_DERIVED_FROM_RELATION)
                    .expect("core/derived-from registered"),
                prepared,
                outcome: WorkspaceOutcome {
                    exit_code: Some(0),
                    stdout_tail: Some("inspected".into()),
                    stderr_tail: None,
                    duration_ms: Some(10),
                },
            })
            .await?;
        let after: i64 = sqlx::query_scalar("SELECT count(*) FROM proxima_core.workspace_run_v1")
            .fetch_one(pg.pool())
            .await?;

        assert!(record.primary_memory_id.is_none());
        assert_eq!(after, before);
        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test(flavor = "multi_thread")]
async fn workspace_run_review_finalize_rejects_inspection_mutations()
-> Result<(), Box<dyn std::error::Error>> {
    let Some((db_name, pg)) = migrated_db().await else {
        return Ok(());
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = test_owner();
        let repo_root = tempfile::tempdir()?;
        let worktree_root = tempfile::tempdir()?;
        let (repo_path, parent_sha) = init_repo(&repo_root).map_err(std::io::Error::other)?;
        let branch_name = format!("proxima/wake/{}", Uuid::now_v7());
        let worker_tree = worktree_root.path().join("worker");
        git(
            &repo_path,
            &[
                "worktree",
                "add",
                "-b",
                &branch_name,
                worker_tree.to_str().expect("utf8 worktree"),
                "main",
            ],
        )
        .map_err(std::io::Error::other)?;
        std::fs::write(worker_tree.join("README.md"), "initial\nverified change\n")?;
        git(&worker_tree, &["add", "README.md"]).map_err(std::io::Error::other)?;
        git(
            &worker_tree,
            &[
                "-c",
                "user.name=Proxima Test",
                "-c",
                "user.email=proxima@example.test",
                "commit",
                "-m",
                "worker change",
            ],
        )
        .map_err(std::io::Error::other)?;
        let head_sha = git(&worker_tree, &["rev-parse", "HEAD"]).map_err(std::io::Error::other)?;

        let repo_id = Uuid::now_v7();
        proxima_code::register_repo(
            pg.pool(),
            &owner,
            repo_id,
            repo_path.to_str().expect("utf8 repo path"),
            "inspect-only-mutation",
        )
        .await?;
        let registry_root = tempfile::tempdir()?;
        let registry = registry_with_runner(&pg, registry_root.path().to_path_buf());
        let request = seed_execution_request_for_runner(
            &pg,
            &owner,
            repo_id,
            "inspect-only-mutation-request",
            "Update README",
            "Update `README.md`.",
        )
        .await?;
        let run_payload = WorkspaceRunV1 {
            wake_invocation_id: Uuid::now_v7(),
            repo_id,
            target_branch: "main".into(),
            worktree_path: worker_tree.to_string_lossy().to_string(),
            branch_name,
            parent_sha,
            head_sha,
            diff_stat_json: WorkspaceDiffStat {
                files_changed: 1,
                insertions: 1,
                deletions: 0,
                files: vec![WorkspaceDiffFile {
                    path: "README.md".into(),
                    insertions: 1,
                    deletions: 0,
                }],
            },
            exit_code: Some(0),
            stdout_tail: Some("worker stdout".into()),
            stderr_tail: None,
            duration_ms: Some(123),
        };
        let run =
            seed_workspace_run_for_runner(&pg, &owner, &registry, &run_payload, request).await?;
        let runner = CodeWorkspaceRunner::new(pg.pool().clone());
        let prepared = runner
            .prepare(WorkspacePrepareInput {
                invocation_id: Uuid::now_v7(),
                owner: &owner,
                wake_token: Uuid::now_v7(),
                mcp_url: "http://127.0.0.1:1/mcp",
                root_perspective_memory_id: proxima_core::MemoryId::new(Uuid::now_v7()),
                triggering_memory_id: run,
                triggering_memory_schema_id: WorkspaceRunV1::SCHEMA_ID,
                triggering_memory_payload: &serde_json::to_value(&run_payload)?,
                is_continuation: false,
                workspace_tool_palette: &[],
            })
            .await?;

        std::fs::write(worker_tree.join("verifier-mutated.txt"), "mutation\n")?;
        let err = match runner
            .finalize(WorkspaceFinalizeInput {
                owner: &owner,
                invocation_id: Uuid::now_v7(),
                wake_entry_id: Uuid::now_v7(),
                personality_instance_id: proxima_core::PersonalityInstanceId::new(Uuid::now_v7()),
                root_perspective_memory_id: proxima_core::MemoryId::new(Uuid::now_v7()),
                triggering_memory_id: run,
                authored_relation: registry
                    .resolve_relation(CORE_AUTHORED_RELATION)
                    .expect("core/authored registered"),
                derived_from_relation: registry
                    .resolve_relation(CORE_DERIVED_FROM_RELATION)
                    .expect("core/derived-from registered"),
                prepared,
                outcome: WorkspaceOutcome {
                    exit_code: Some(0),
                    stdout_tail: Some("inspected".into()),
                    stderr_tail: None,
                    duration_ms: Some(10),
                },
            })
            .await
        {
            Ok(_) => panic!("inspect-only finalize accepted a worktree mutation"),
            Err(err) => err,
        };
        match err {
            WorkspaceRunnerError::FinalizeFailed(reason) => {
                assert!(reason.contains("workspace_inspect_modified_worktree"));
            }
            other => panic!("unexpected error: {other}"),
        }
        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test(flavor = "multi_thread")]
async fn workspace_run_trigger_resolves_original_request_through_run_chain()
-> Result<(), Box<dyn std::error::Error>> {
    let Some((db_name, pg)) = migrated_db().await else {
        return Ok(());
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = test_owner();
        let repo_root = tempfile::tempdir()?;
        let worktree_root = tempfile::tempdir()?;
        let (repo_path, parent_sha) = init_repo(&repo_root).map_err(std::io::Error::other)?;
        let branch_name = format!("proxima/wake/{}", Uuid::now_v7());
        let worker_tree = worktree_root.path().join("worker");
        git(
            &repo_path,
            &[
                "worktree",
                "add",
                "-b",
                &branch_name,
                worker_tree.to_str().expect("utf8 worktree"),
                "main",
            ],
        )
        .map_err(std::io::Error::other)?;
        std::fs::write(worker_tree.join("README.md"), "initial\nchain change\n")?;
        git(&worker_tree, &["add", "README.md"]).map_err(std::io::Error::other)?;
        git(
            &worker_tree,
            &[
                "-c",
                "user.name=Proxima Test",
                "-c",
                "user.email=proxima@example.test",
                "commit",
                "-m",
                "chain change",
            ],
        )
        .map_err(std::io::Error::other)?;
        let head_sha = git(&worker_tree, &["rev-parse", "HEAD"]).map_err(std::io::Error::other)?;

        let repo_id = Uuid::now_v7();
        proxima_code::register_repo(
            pg.pool(),
            &owner,
            repo_id,
            repo_path.to_str().expect("utf8 repo path"),
            "chained-review-context",
        )
        .await?;
        let registry_root = tempfile::tempdir()?;
        let registry = registry_with_runner(&pg, registry_root.path().to_path_buf());
        let request = seed_execution_request_for_runner(
            &pg,
            &owner,
            repo_id,
            "chained-context-request",
            "Update README",
            "Update `README.md`.",
        )
        .await?;
        let first_run_payload = WorkspaceRunV1 {
            wake_invocation_id: Uuid::now_v7(),
            repo_id,
            target_branch: "main".into(),
            worktree_path: worker_tree.to_string_lossy().to_string(),
            branch_name: branch_name.clone(),
            parent_sha: parent_sha.clone(),
            head_sha: head_sha.clone(),
            diff_stat_json: WorkspaceDiffStat {
                files_changed: 1,
                insertions: 1,
                deletions: 0,
                files: vec![WorkspaceDiffFile {
                    path: "README.md".into(),
                    insertions: 1,
                    deletions: 0,
                }],
            },
            exit_code: Some(0),
            stdout_tail: None,
            stderr_tail: None,
            duration_ms: Some(100),
        };
        let first_run =
            seed_workspace_run_for_runner(&pg, &owner, &registry, &first_run_payload, request)
                .await?;
        let chained_run_payload = WorkspaceRunV1 {
            wake_invocation_id: Uuid::now_v7(),
            repo_id,
            target_branch: "main".into(),
            worktree_path: worker_tree.to_string_lossy().to_string(),
            branch_name,
            parent_sha,
            head_sha,
            diff_stat_json: first_run_payload.diff_stat_json.clone(),
            exit_code: Some(0),
            stdout_tail: None,
            stderr_tail: None,
            duration_ms: Some(101),
        };
        let chained_run =
            seed_workspace_run_for_runner(&pg, &owner, &registry, &chained_run_payload, first_run)
                .await?;

        let runner = CodeWorkspaceRunner::new(pg.pool().clone());
        let prepared = runner
            .prepare(WorkspacePrepareInput {
                invocation_id: Uuid::now_v7(),
                owner: &owner,
                wake_token: Uuid::now_v7(),
                mcp_url: "http://127.0.0.1:1/mcp",
                root_perspective_memory_id: proxima_core::MemoryId::new(Uuid::now_v7()),
                triggering_memory_id: chained_run,
                triggering_memory_schema_id: WorkspaceRunV1::SCHEMA_ID,
                triggering_memory_payload: &serde_json::to_value(&chained_run_payload)?,
                is_continuation: false,
                workspace_tool_palette: &[],
            })
            .await?;

        let context = prepared.workspace_context.expect("workspace context");
        assert_eq!(
            context["original_request"]["memory_id"],
            request.into_inner().to_string()
        );
        assert_eq!(
            context["original_request"]["payload"]["request_key"],
            "chained-context-request"
        );
        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test(flavor = "multi_thread")]
async fn workspace_run_trigger_loads_goal_context_from_request_chain()
-> Result<(), Box<dyn std::error::Error>> {
    let Some((db_name, pg)) = migrated_db().await else {
        return Ok(());
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        proxima_flavor_goal::migrator().run(pg.pool()).await?;
        let owner = test_owner();
        let repo_root = tempfile::tempdir()?;
        let worktree_root = tempfile::tempdir()?;
        let (repo_path, parent_sha) = init_repo(&repo_root).map_err(std::io::Error::other)?;
        let branch_name = format!("proxima/wake/{}", Uuid::now_v7());
        let worker_tree = worktree_root.path().join("worker");
        git(
            &repo_path,
            &[
                "worktree",
                "add",
                "-b",
                &branch_name,
                worker_tree.to_str().expect("utf8 worktree"),
                "main",
            ],
        )
        .map_err(std::io::Error::other)?;
        std::fs::write(worker_tree.join("README.md"), "initial\ngoal change\n")?;
        git(&worker_tree, &["add", "README.md"]).map_err(std::io::Error::other)?;
        git(
            &worker_tree,
            &[
                "-c",
                "user.name=Proxima Test",
                "-c",
                "user.email=proxima@example.test",
                "commit",
                "-m",
                "goal change",
            ],
        )
        .map_err(std::io::Error::other)?;
        let head_sha = git(&worker_tree, &["rev-parse", "HEAD"]).map_err(std::io::Error::other)?;

        let repo_id = Uuid::now_v7();
        proxima_code::register_repo(
            pg.pool(),
            &owner,
            repo_id,
            repo_path.to_str().expect("utf8 repo path"),
            "goal-review-context",
        )
        .await?;
        let registry_root = tempfile::tempdir()?;
        let registry = registry_with_runner(&pg, registry_root.path().to_path_buf());
        let request = seed_execution_request_for_runner(
            &pg,
            &owner,
            repo_id,
            "goal-context-request",
            "Close verifier loop",
            "Implement the verifier closure slice.",
        )
        .await?;
        let (goal_id, goal_activation) =
            seed_goal_context_for_runner(&pg, &owner, request, &registry).await?;
        let correction_request = seed_execution_request_for_runner(
            &pg,
            &owner,
            repo_id,
            "goal-context-request:correction:0",
            "Correct verifier loop",
            "Finish the verifier closure slice.",
        )
        .await?;
        append_derived_edge_for_runner(&pg, &owner, &registry, correction_request, request).await?;

        let run_payload = WorkspaceRunV1 {
            wake_invocation_id: Uuid::now_v7(),
            repo_id,
            target_branch: "main".into(),
            worktree_path: worker_tree.to_string_lossy().to_string(),
            branch_name,
            parent_sha,
            head_sha,
            diff_stat_json: WorkspaceDiffStat {
                files_changed: 1,
                insertions: 1,
                deletions: 0,
                files: vec![WorkspaceDiffFile {
                    path: "README.md".into(),
                    insertions: 1,
                    deletions: 0,
                }],
            },
            exit_code: Some(0),
            stdout_tail: None,
            stderr_tail: None,
            duration_ms: Some(100),
        };
        let run =
            seed_workspace_run_for_runner(&pg, &owner, &registry, &run_payload, correction_request)
                .await?;

        let runner = CodeWorkspaceRunner::new(pg.pool().clone());
        let prepared = runner
            .prepare(WorkspacePrepareInput {
                invocation_id: Uuid::now_v7(),
                owner: &owner,
                wake_token: Uuid::now_v7(),
                mcp_url: "http://127.0.0.1:1/mcp",
                root_perspective_memory_id: proxima_core::MemoryId::new(Uuid::now_v7()),
                triggering_memory_id: run,
                triggering_memory_schema_id: WorkspaceRunV1::SCHEMA_ID,
                triggering_memory_payload: &serde_json::to_value(&run_payload)?,
                is_continuation: false,
                workspace_tool_palette: &[],
            })
            .await?;

        let context = prepared.workspace_context.expect("workspace context");
        assert_eq!(
            context["original_request"]["memory_id"],
            request.into_inner().to_string()
        );
        assert_eq!(
            context["active_goal"]["activated_memory_id"],
            goal_activation.into_inner().to_string()
        );
        assert_eq!(
            context["active_goal"]["head"]["goal_id"],
            goal_id.to_string()
        );
        assert_eq!(context["active_goal"]["head"]["state"], "Active");
        assert!(
            context["active_goal"]["head"]["text"]
                .as_str()
                .unwrap_or_default()
                .contains("Acceptance criteria")
        );
        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test(flavor = "multi_thread")]
async fn correction_execution_request_reuses_derived_workspace_run_worktree()
-> Result<(), Box<dyn std::error::Error>> {
    let Some((db_name, pg)) = migrated_db().await else {
        return Ok(());
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = test_owner();
        let repo_root = tempfile::tempdir()?;
        let worktree_root = tempfile::tempdir()?;
        let (repo_path, parent_sha) = init_repo(&repo_root).map_err(std::io::Error::other)?;
        let branch_name = format!("proxima/wake/{}", Uuid::now_v7());
        let worker_tree = worktree_root.path().join("worker");
        git(
            &repo_path,
            &[
                "worktree",
                "add",
                "-b",
                &branch_name,
                worker_tree.to_str().expect("utf8 worktree"),
                "main",
            ],
        )
        .map_err(std::io::Error::other)?;
        std::fs::write(worker_tree.join("README.md"), "initial\nfirst pass\n")?;
        git(&worker_tree, &["add", "README.md"]).map_err(std::io::Error::other)?;
        git(
            &worker_tree,
            &[
                "-c",
                "user.name=Proxima Test",
                "-c",
                "user.email=proxima@example.test",
                "commit",
                "-m",
                "first pass",
            ],
        )
        .map_err(std::io::Error::other)?;
        let head_sha = git(&worker_tree, &["rev-parse", "HEAD"]).map_err(std::io::Error::other)?;

        let repo_id = Uuid::now_v7();
        proxima_code::register_repo(
            pg.pool(),
            &owner,
            repo_id,
            repo_path.to_str().expect("utf8 repo path"),
            "continuation-context",
        )
        .await?;
        let registry_root = tempfile::tempdir()?;
        let registry = registry_with_runner(&pg, registry_root.path().to_path_buf());
        let request = seed_execution_request_for_runner(
            &pg,
            &owner,
            repo_id,
            "continuation-request",
            "Update README",
            "Update `README.md`.",
        )
        .await?;
        let run_payload = WorkspaceRunV1 {
            wake_invocation_id: Uuid::now_v7(),
            repo_id,
            target_branch: "main".into(),
            worktree_path: worker_tree.to_string_lossy().to_string(),
            branch_name: branch_name.clone(),
            parent_sha: parent_sha.clone(),
            head_sha: head_sha.clone(),
            diff_stat_json: WorkspaceDiffStat {
                files_changed: 1,
                insertions: 1,
                deletions: 0,
                files: vec![WorkspaceDiffFile {
                    path: "README.md".into(),
                    insertions: 1,
                    deletions: 0,
                }],
            },
            exit_code: Some(0),
            stdout_tail: None,
            stderr_tail: None,
            duration_ms: Some(100),
        };
        let run =
            seed_workspace_run_for_runner(&pg, &owner, &registry, &run_payload, request).await?;
        let correction_request = seed_execution_request_for_runner(
            &pg,
            &owner,
            repo_id,
            "continuation-request:correction:0",
            "Correct README",
            "Continue in the existing workspace.",
        )
        .await?;
        append_derived_edge_for_runner(&pg, &owner, &registry, correction_request, run).await?;

        let payload = serde_json::to_value(ExecutionRequestV1 {
            repo_id,
            title: "Correct README".into(),
            instructions: "Continue in the existing workspace.".into(),
            request_key: "continuation-request:correction:0".into(),
        })?;
        let runner = CodeWorkspaceRunner::new(pg.pool().clone())
            .with_worktrees_root(worktree_root.path().join("new-worktrees"));
        let continuation_payload = serde_json::to_value(ExecutionRequestV1 {
            repo_id,
            title: "Update README".into(),
            instructions: "Update `README.md`.".into(),
            request_key: "continuation-request".into(),
        })?;
        let continued = runner
            .prepare(WorkspacePrepareInput {
                invocation_id: Uuid::now_v7(),
                owner: &owner,
                wake_token: Uuid::now_v7(),
                mcp_url: "http://127.0.0.1:1/mcp",
                root_perspective_memory_id: proxima_core::MemoryId::new(Uuid::now_v7()),
                triggering_memory_id: request,
                triggering_memory_schema_id: ExecutionRequestV1::SCHEMA_ID,
                triggering_memory_payload: &continuation_payload,
                is_continuation: true,
                workspace_tool_palette: &[],
            })
            .await?;

        assert_eq!(continued.work_dir, worker_tree);
        let continued_context = continued.workspace_context.expect("workspace context");
        assert_eq!(continued_context["mode"], "continue_execution_request");
        assert_eq!(
            continued_context["continuation_from"]["workspace_run_memory_id"],
            run.into_inner().to_string()
        );

        let prepared = runner
            .prepare(WorkspacePrepareInput {
                invocation_id: Uuid::now_v7(),
                owner: &owner,
                wake_token: Uuid::now_v7(),
                mcp_url: "http://127.0.0.1:1/mcp",
                root_perspective_memory_id: proxima_core::MemoryId::new(Uuid::now_v7()),
                triggering_memory_id: correction_request,
                triggering_memory_schema_id: ExecutionRequestV1::SCHEMA_ID,
                triggering_memory_payload: &payload,
                is_continuation: false,
                workspace_tool_palette: &[],
            })
            .await?;

        assert_eq!(prepared.work_dir, worker_tree);
        assert_eq!(
            git(&prepared.work_dir, &["rev-parse", "HEAD"]).map_err(std::io::Error::other)?,
            head_sha
        );
        let context = prepared.workspace_context.expect("workspace context");
        assert_eq!(context["mode"], "continue_execution_request");
        assert_eq!(
            context["continuation_from"]["workspace_run_memory_id"],
            run.into_inner().to_string()
        );
        assert_eq!(context["continuation_from"]["branch_name"], branch_name);
        assert_eq!(context["continuation_from"]["head_sha"], head_sha);
        assert_eq!(context["parent_sha"], parent_sha);
        assert_eq!(context["tooling"]["frontend"]["pnpm"]["status"], "reused");
        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test(flavor = "multi_thread")]
async fn rejected_review_trigger_prepares_correction_context()
-> Result<(), Box<dyn std::error::Error>> {
    let Some((db_name, pg)) = migrated_db().await else {
        return Ok(());
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = test_owner();
        let repo_root = tempfile::tempdir()?;
        let worktree_root = tempfile::tempdir()?;
        let (repo_path, parent_sha) = init_repo(&repo_root).map_err(std::io::Error::other)?;
        let worker_tree = worktree_root.path().join("worker");
        let branch_name = format!("proxima/wake/{}", Uuid::now_v7());
        git(
            &repo_path,
            &[
                "worktree",
                "add",
                "-b",
                &branch_name,
                worker_tree.to_str().expect("utf8 worktree"),
                "main",
            ],
        )
        .map_err(std::io::Error::other)?;
        std::fs::write(worker_tree.join("README.md"), "initial\nwrong change\n")?;
        git(&worker_tree, &["add", "README.md"]).map_err(std::io::Error::other)?;
        git(
            &worker_tree,
            &[
                "-c",
                "user.name=Proxima Test",
                "-c",
                "user.email=proxima@example.test",
                "commit",
                "-m",
                "worker wrong change",
            ],
        )
        .map_err(std::io::Error::other)?;
        let head_sha = git(&worker_tree, &["rev-parse", "HEAD"]).map_err(std::io::Error::other)?;

        let repo_id = Uuid::now_v7();
        proxima_code::register_repo(
            pg.pool(),
            &owner,
            repo_id,
            repo_path.to_str().expect("utf8 repo path"),
            "correction-context",
        )
        .await?;
        let registry_root = tempfile::tempdir()?;
        let registry = registry_with_runner(&pg, registry_root.path().to_path_buf());
        let request = seed_execution_request_for_runner(
            &pg,
            &owner,
            repo_id,
            "correction-context-request",
            "Update README",
            "Update `README.md` with the accepted text.",
        )
        .await?;
        let run_payload = WorkspaceRunV1 {
            wake_invocation_id: Uuid::now_v7(),
            repo_id,
            target_branch: "main".into(),
            worktree_path: worker_tree.to_string_lossy().to_string(),
            branch_name,
            parent_sha,
            head_sha,
            diff_stat_json: WorkspaceDiffStat {
                files_changed: 1,
                insertions: 1,
                deletions: 0,
                files: vec![WorkspaceDiffFile {
                    path: "README.md".into(),
                    insertions: 1,
                    deletions: 0,
                }],
            },
            exit_code: Some(0),
            stdout_tail: None,
            stderr_tail: None,
            duration_ms: Some(234),
        };
        let run =
            seed_workspace_run_for_runner(&pg, &owner, &registry, &run_payload, request).await?;
        let review_payload = WorkspaceReviewV1 {
            workspace_run_memory_id: run.into_inner(),
            execution_request_memory_id: request.into_inner(),
            verdict: WorkspaceReviewVerdict::Rejected,
            round_index: 0,
            summary: "Implementation uses the wrong text.".into(),
            findings: vec![WorkspaceReviewFinding {
                severity: "major".into(),
                file_path: Some("README.md".into()),
                line: Some(2),
                message: "Replace wrong change with accepted text.".into(),
            }],
            correction_instructions: Some("Use the accepted text in README.md.".into()),
            verification_summary: Some("manual diff review failed".into()),
            reviewed_at: time::OffsetDateTime::now_utc(),
        };
        let review = seed_workspace_review_for_runner(&pg, &owner, &review_payload).await?;
        let payload = serde_json::to_value(&review_payload)?;
        let runner = CodeWorkspaceRunner::new(pg.pool().clone());
        let prepared = runner
            .prepare(WorkspacePrepareInput {
                invocation_id: Uuid::now_v7(),
                owner: &owner,
                wake_token: Uuid::now_v7(),
                mcp_url: "http://127.0.0.1:1/mcp",
                root_perspective_memory_id: proxima_core::MemoryId::new(Uuid::now_v7()),
                triggering_memory_id: review,
                triggering_memory_schema_id: WorkspaceReviewV1::SCHEMA_ID,
                triggering_memory_payload: &payload,
                is_continuation: false,
                workspace_tool_palette: &[],
            })
            .await?;

        let context = prepared.workspace_context.expect("workspace context");
        assert_eq!(context["mode"], "plan_workspace_correction");
        assert_eq!(context["trigger_kind"], "workspace_review");
        assert_eq!(
            context["original_request"]["payload"]["request_key"],
            "correction-context-request"
        );
        assert_eq!(
            context["rejected_review"]["correction_instructions"],
            "Use the accepted text in README.md."
        );
        assert_eq!(context["prior_reviews"].as_array().unwrap().len(), 1);
        assert_eq!(context["prior_reviews"][0]["verdict"], "rejected");
        assert_eq!(context["prior_decisions"].as_array().unwrap().len(), 0);
        assert_eq!(context["veto_count"], 1);
        assert_eq!(context["max_veto_rounds"], 2);
        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test(flavor = "multi_thread")]
async fn rejected_decision_trigger_is_not_correction_context()
-> Result<(), Box<dyn std::error::Error>> {
    let Some((db_name, pg)) = migrated_db().await else {
        return Ok(());
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = test_owner();
        let repo_root = tempfile::tempdir()?;
        let worktree_root = tempfile::tempdir()?;
        let (repo_path, parent_sha) = init_repo(&repo_root).map_err(std::io::Error::other)?;

        let repo_id = Uuid::now_v7();
        proxima_code::register_repo(
            pg.pool(),
            &owner,
            repo_id,
            repo_path.to_str().expect("utf8 repo path"),
            "discard-context",
        )
        .await?;
        let registry_root = tempfile::tempdir()?;
        let registry = registry_with_runner(&pg, registry_root.path().to_path_buf());
        let request = seed_execution_request_for_runner(
            &pg,
            &owner,
            repo_id,
            "discard-context-request",
            "Update README",
            "Update `README.md` with the accepted text.",
        )
        .await?;
        let run_payload = WorkspaceRunV1 {
            wake_invocation_id: Uuid::now_v7(),
            repo_id,
            target_branch: "main".into(),
            worktree_path: worktree_root.path().to_string_lossy().to_string(),
            branch_name: "proxima/wake/discard".into(),
            parent_sha: parent_sha.clone(),
            head_sha: parent_sha,
            diff_stat_json: WorkspaceDiffStat {
                files_changed: 0,
                insertions: 0,
                deletions: 0,
                files: Vec::new(),
            },
            exit_code: Some(0),
            stdout_tail: None,
            stderr_tail: None,
            duration_ms: Some(1),
        };
        let run =
            seed_workspace_run_for_runner(&pg, &owner, &registry, &run_payload, request).await?;
        let decision = proxima_code::emit_workspace_decision(
            pg.pool(),
            &owner,
            run,
            WorkspaceDecision::Rejected,
            Some("discard this run"),
        )
        .await?;
        let decision_payload = WorkspaceDecisionV1 {
            workspace_run_memory_id: run.into_inner(),
            decision: WorkspaceDecision::Rejected,
            decided_at: time::OffsetDateTime::now_utc(),
            reason_text: Some("discard this run".into()),
            decided_by_owner_id: match &owner.principal {
                Principal::User(user) => user.into_inner(),
                Principal::Group(group) => group.into_inner(),
            },
        };
        let payload = serde_json::to_value(&decision_payload)?;
        let runner = CodeWorkspaceRunner::new(pg.pool().clone());
        #[allow(clippy::manual_let_else)]
        let err = match runner
            .prepare(WorkspacePrepareInput {
                invocation_id: Uuid::now_v7(),
                owner: &owner,
                wake_token: Uuid::now_v7(),
                mcp_url: "http://127.0.0.1:1/mcp",
                root_perspective_memory_id: proxima_core::MemoryId::new(Uuid::now_v7()),
                triggering_memory_id: decision,
                triggering_memory_schema_id: WorkspaceDecisionV1::SCHEMA_ID,
                triggering_memory_payload: &payload,
                is_continuation: false,
                workspace_tool_palette: &[],
            })
            .await
        {
            Ok(_) => panic!("discard decisions should not plan corrections"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("no workspace prep"));
        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test(flavor = "multi_thread")]
async fn merged_decision_trigger_prepares_goal_close_context()
-> Result<(), Box<dyn std::error::Error>> {
    let Some((db_name, pg)) = migrated_db().await else {
        return Ok(());
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        proxima_flavor_goal::migrator().run(pg.pool()).await?;
        let owner = test_owner();
        let repo_root = tempfile::tempdir()?;
        let (repo_path, parent_sha) = init_repo(&repo_root).map_err(std::io::Error::other)?;

        let repo_id = Uuid::now_v7();
        proxima_code::register_repo(
            pg.pool(),
            &owner,
            repo_id,
            repo_path.to_str().expect("utf8 repo path"),
            "merge-goal-close-context",
        )
        .await?;
        let registry_root = tempfile::tempdir()?;
        let registry = registry_with_runner(&pg, registry_root.path().to_path_buf());
        let request = seed_execution_request_for_runner(
            &pg,
            &owner,
            repo_id,
            "merge-goal-close-request",
            "Close Goal after merge",
            "Implement the full accepted Goal closure.",
        )
        .await?;
        let (goal_id, goal_activation) =
            seed_goal_context_for_runner(&pg, &owner, request, &registry).await?;
        let run_payload = WorkspaceRunV1 {
            wake_invocation_id: Uuid::now_v7(),
            repo_id,
            target_branch: "main".into(),
            worktree_path: repo_path.to_string_lossy().to_string(),
            branch_name: "main".into(),
            parent_sha: parent_sha.clone(),
            head_sha: parent_sha,
            diff_stat_json: WorkspaceDiffStat {
                files_changed: 0,
                insertions: 0,
                deletions: 0,
                files: Vec::new(),
            },
            exit_code: Some(0),
            stdout_tail: None,
            stderr_tail: None,
            duration_ms: Some(1),
        };
        let run =
            seed_workspace_run_for_runner(&pg, &owner, &registry, &run_payload, request).await?;
        let review_payload = WorkspaceReviewV1 {
            workspace_run_memory_id: run.into_inner(),
            execution_request_memory_id: request.into_inner(),
            verdict: WorkspaceReviewVerdict::Approved,
            round_index: 0,
            summary: "approved".into(),
            findings: Vec::<WorkspaceReviewFinding>::new(),
            correction_instructions: None,
            verification_summary: Some("checked".into()),
            reviewed_at: time::OffsetDateTime::now_utc(),
        };
        seed_workspace_review_for_runner(&pg, &owner, &review_payload).await?;
        let decision = proxima_code::emit_workspace_decision(
            pg.pool(),
            &owner,
            run,
            WorkspaceDecision::Merged,
            None,
        )
        .await?;
        let decision_payload = WorkspaceDecisionV1 {
            workspace_run_memory_id: run.into_inner(),
            decision: WorkspaceDecision::Merged,
            decided_at: time::OffsetDateTime::now_utc(),
            reason_text: None,
            decided_by_owner_id: match &owner.principal {
                Principal::User(user) => user.into_inner(),
                Principal::Group(group) => group.into_inner(),
            },
        };
        let runner = CodeWorkspaceRunner::new(pg.pool().clone());
        let prepared = runner
            .prepare(WorkspacePrepareInput {
                invocation_id: Uuid::now_v7(),
                owner: &owner,
                wake_token: Uuid::now_v7(),
                mcp_url: "http://127.0.0.1:1/mcp",
                root_perspective_memory_id: proxima_core::MemoryId::new(Uuid::now_v7()),
                triggering_memory_id: decision,
                triggering_memory_schema_id: WorkspaceDecisionV1::SCHEMA_ID,
                triggering_memory_payload: &serde_json::to_value(&decision_payload)?,
                is_continuation: false,
                workspace_tool_palette: &[],
            })
            .await?;

        let context = prepared.workspace_context.expect("workspace context");
        assert_eq!(context["mode"], "close_goal_after_merge");
        assert_eq!(context["merged_decision"]["decision"], "merged");
        assert_eq!(
            context["active_goal"]["activated_memory_id"],
            goal_activation.into_inner().to_string()
        );
        assert_eq!(
            context["active_goal"]["head"]["goal_id"],
            goal_id.to_string()
        );
        assert_eq!(context["latest_review"]["verdict"], "approved");
        assert_eq!(context["goal_close"]["status"], "ready");
        assert_eq!(context["goal_close"]["goal_id"], goal_id.to_string());
        assert_eq!(
            context["goal_close"]["evidence_memory_ids"][0],
            decision.into_inner().to_string()
        );

        let mut goal_registry = FlavorRegistry::new();
        proxima_flavor_goal::register(&mut goal_registry);
        let goal_ctx = McpToolCtx {
            pool: pg.pool().clone(),
            owner: owner.clone(),
            handles: None,
            mode: OutputMode::RawIds,
            registry: Arc::new(goal_registry.freeze()),
            author: McpAuthorContext {
                model_id: "test-model".into(),
                client_name: "test".into(),
                client_version: "1".into(),
                caller_self_perspective: None,
            },
            caller_self_perspective: None,
            master_token_id: None,
            engine: None,
        };
        let close = MarkAchievedTool::call(
            goal_ctx.clone(),
            MarkAchievedArgs {
                goal: context["goal_close"]["goal_id"]
                    .as_str()
                    .expect("goal id")
                    .to_string(),
                evidence: vec![decision.into_inner().to_string()],
                idempotency_key: Some(format!("goal-close:{}", decision.into_inner())),
            },
        )
        .await?;
        assert!(matches!(close.status, MarkAchievedStatus::Achieved));
        let achieved_id = goal_ctx
            .resolve_goal(close.handle.as_deref().expect("achieved handle"))
            .expect("achieved handle resolves")
            .into_inner();
        let row: (GoalState, Option<Uuid>) =
            sqlx::query_as("SELECT state, supersedes FROM proxima_core.goals WHERE goal_id = $1")
                .bind(achieved_id)
                .fetch_one(pg.pool())
                .await?;
        assert_eq!(row.0, GoalState::Achieved);
        assert_eq!(row.1, Some(goal_id));
        let achieved_fact_count: i64 = sqlx::query_scalar(
            "SELECT count(*)
               FROM proxima_goal.goal_achieved_v1
              WHERE goal_id = $1",
        )
        .bind(achieved_id)
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(achieved_fact_count, 1);
        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test(flavor = "multi_thread")]
async fn retry_requested_decision_trigger_prepares_correction_context()
-> Result<(), Box<dyn std::error::Error>> {
    let Some((db_name, pg)) = migrated_db().await else {
        return Ok(());
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = test_owner();
        let repo_root = tempfile::tempdir()?;
        let (repo_path, parent_sha) = init_repo(&repo_root).map_err(std::io::Error::other)?;

        let repo_id = Uuid::now_v7();
        proxima_code::register_repo(
            pg.pool(),
            &owner,
            repo_id,
            repo_path.to_str().expect("utf8 repo path"),
            "retry-context",
        )
        .await?;
        let registry_root = tempfile::tempdir()?;
        let registry = registry_with_runner(&pg, registry_root.path().to_path_buf());
        let request = seed_execution_request_for_runner(
            &pg,
            &owner,
            repo_id,
            "retry-context-request",
            "Update README",
            "Update `README.md` with the accepted text.",
        )
        .await?;
        let run_payload = WorkspaceRunV1 {
            wake_invocation_id: Uuid::now_v7(),
            repo_id,
            target_branch: "main".into(),
            worktree_path: repo_path.to_string_lossy().to_string(),
            branch_name: "main".into(),
            parent_sha: parent_sha.clone(),
            head_sha: parent_sha,
            diff_stat_json: WorkspaceDiffStat {
                files_changed: 0,
                insertions: 0,
                deletions: 0,
                files: Vec::new(),
            },
            exit_code: Some(0),
            stdout_tail: None,
            stderr_tail: None,
            duration_ms: Some(1),
        };
        let run =
            seed_workspace_run_for_runner(&pg, &owner, &registry, &run_payload, request).await?;
        let decision = proxima_code::emit_workspace_decision(
            pg.pool(),
            &owner,
            run,
            WorkspaceDecision::RetryRequested,
            Some("try the smaller fix"),
        )
        .await?;
        let decision_payload = WorkspaceDecisionV1 {
            workspace_run_memory_id: run.into_inner(),
            decision: WorkspaceDecision::RetryRequested,
            decided_at: time::OffsetDateTime::now_utc(),
            reason_text: Some("try the smaller fix".into()),
            decided_by_owner_id: match &owner.principal {
                Principal::User(user) => user.into_inner(),
                Principal::Group(group) => group.into_inner(),
            },
        };
        let payload = serde_json::to_value(&decision_payload)?;
        let runner = CodeWorkspaceRunner::new(pg.pool().clone());
        let prepared = runner
            .prepare(WorkspacePrepareInput {
                invocation_id: Uuid::now_v7(),
                owner: &owner,
                wake_token: Uuid::now_v7(),
                mcp_url: "http://127.0.0.1:1/mcp",
                root_perspective_memory_id: proxima_core::MemoryId::new(Uuid::now_v7()),
                triggering_memory_id: decision,
                triggering_memory_schema_id: WorkspaceDecisionV1::SCHEMA_ID,
                triggering_memory_payload: &payload,
                is_continuation: false,
                workspace_tool_palette: &[],
            })
            .await?;

        let context = prepared.workspace_context.expect("workspace context");
        assert_eq!(context["mode"], "plan_workspace_correction");
        assert_eq!(context["trigger_kind"], "workspace_decision");
        assert_eq!(
            context["retry_requested_decision"]["decision"],
            "retry_requested"
        );
        assert_eq!(context["latest_rejected_review"], serde_json::Value::Null);
        assert_eq!(context["veto_count"], 1);
        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test(flavor = "multi_thread")]
async fn workspace_wake_emits_run_fact_and_edges() -> Result<(), Box<dyn std::error::Error>> {
    let Some((db_name, pg)) = migrated_db().await else {
        return Ok(());
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = test_owner();
        let repo_root = tempfile::tempdir()?;
        let worktrees_root = tempfile::tempdir()?;
        let (repo_path, commit_sha) = init_repo(&repo_root).map_err(std::io::Error::other)?;
        let repo_id = Uuid::now_v7();
        let repo = proxima_code::register_repo(
            pg.pool(),
            &owner,
            repo_id,
            repo_path.to_str().expect("utf8 repo path"),
            "workspace-smoke",
        )
        .await?;
        assert_eq!(repo.target_branch.as_deref(), Some("main"));
        sqlx::query("UPDATE proxima_code.repos SET target_branch = NULL WHERE repo_id = $1")
            .bind(repo_id)
            .execute(pg.pool())
            .await?;

        let engine = Arc::new(
            Engine::new(
                registry_with_runner(&pg, worktrees_root.path().to_path_buf()),
                MemoryStore::new(),
                Box::new(NoAuth::new(owner.principal.clone(), owner.clone())),
            )
            .with_storage(Arc::new(pg.clone()))
            .with_embed(Arc::new(FakeEmbedding)),
        );
        engine
            .set_mcp_url("http://127.0.0.1:1/mcp".to_string())
            .await;
        engine
            .set_target_adapter(Arc::new(WorktreeWritingAdapter))
            .await;

        pg.register_inference_target(&RegisterInferenceTargetRequest {
            owner: owner.clone(),
            target_ref: "test/workspace-adapter".into(),
            config: InferenceTargetConfig::MistralChat(MistralChatConfig {
                base_url: "http://127.0.0.1:9".into(),
                model_id: "test-model".into(),
                api_key_env: "PATH".into(),
                temperature: None,
                max_completion_tokens: None,
                reasoning_effort: None,

                context_window_tokens: None,
            }),
        })
        .await?;
        pg.bind_inference_tier(&BindInferenceTierRequest {
            owner: owner.clone(),
            tier: ModelTier::Standard,
            target_ref: "test/workspace-adapter".into(),
        })
        .await?;

        let executor = engine
            .instantiate_personality(InstantiatePersonalityRequest {
                owner: owner.clone(),
                display_name: "Workspace Executor".into(),
                purpose: "Smoke-test workspace runner".into(),
            })
            .await?;
        let mut wake_entry = WakeEntryDraft::new(
            Uuid::now_v7(),
            executor.instance_id,
            WakeEntryTriggerKind::OnMemory,
            <CommitV1 as proxima_core::FactPayload>::SCHEMA_ID,
            "workspace-smoke",
            WakeEntryAuthoredBy::Other,
            1000,
            ModelTier::Standard,
            None,
            Vec::new(),
            1,
        )?;
        wake_entry.execution_mode = WakeExecutionMode::Workspace;
        wake_entry.workspace_tool_palette = vec!["proxima-workspace/shell".into()];
        pg.set_wake_entries(&SetWakeEntriesRequest {
            owner: owner.clone(),
            personality_instance_id: executor.instance_id,
            entries: vec![wake_entry.clone()],
        })
        .await?;
        let runtime = pg
            .fetch_personality_runtime(&owner, executor.instance_id)
            .await?
            .expect("executor runtime row");
        let executor_root = runtime.current_root_perspective_memory_id;

        let commit_payload = CommitV1 {
            repo_id,
            sha: commit_sha.clone(),
            parents: Vec::new(),
            author_name: "Proxima Test".into(),
            author_email: "proxima@example.test".into(),
            author_time: time::OffsetDateTime::now_utc(),
            committer_name: "Proxima Test".into(),
            committer_email: "proxima@example.test".into(),
            committer_time: time::OffsetDateTime::now_utc(),
            message: "initial".into(),
        };
        let commit_outcome = proxima_code::ingest_commit(
            pg.pool(),
            &owner,
            SourceBatchId::new(Uuid::now_v7()),
            &commit_payload,
            time::OffsetDateTime::now_utc(),
        )
        .await?;

        let fired = engine.run_dispatcher_tick().await?;
        assert_eq!(fired, 1, "commit Fact fires workspace wake");

        let invocation_status: proxima_core::WakeInvocationStatus = sqlx::query_scalar(
            "SELECT status
             FROM proxima_core.personality_wake_invocations
             WHERE personality_instance_id = $1
               AND wake_entry_id = $2",
        )
        .bind(executor.instance_id.into_inner())
        .bind(wake_entry.wake_entry_id)
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(
            invocation_status,
            proxima_core::WakeInvocationStatus::Succeeded
        );

        let (invocation_id, session_log_path): (Uuid, String) = sqlx::query_as(
            "SELECT i.invocation_id, l.message_tail
             FROM proxima_core.personality_wake_invocations i
             JOIN proxima_core.personality_wake_invocation_logs l
               ON l.invocation_id = i.invocation_id
             WHERE i.personality_instance_id = $1
               AND i.wake_entry_id = $2
               AND l.phase = 'session_artifact'
               AND l.status = 'started'
             ORDER BY l.log_seq ASC
             LIMIT 1",
        )
        .bind(executor.instance_id.into_inner())
        .bind(wake_entry.wake_entry_id)
        .fetch_one(pg.pool())
        .await?;
        assert_ne!(invocation_id, Uuid::nil());
        let session_log = tokio::fs::read_to_string(&session_log_path).await?;
        assert!(
            session_log.contains(r#""record":"workspace-smoke""#),
            "workspace-mode wake should mirror harness JSONL to {session_log_path}: {session_log}"
        );

        let persisted_target: Option<String> =
            sqlx::query_scalar("SELECT target_branch FROM proxima_code.repos WHERE repo_id = $1")
                .bind(repo_id)
                .fetch_one(pg.pool())
                .await?;
        assert_eq!(persisted_target.as_deref(), Some("main"));

        let run = sqlx::query(
            "SELECT wr.memory_id, wr.base_ref AS target_branch, wr.worktree_path,
                    wr.parent_sha, wr.head_sha, wr.diff_stat_json
             FROM proxima_core.workspace_run_v1 wr
             JOIN proxima_core.edges e
               ON e.source_memory_id = wr.memory_id
              AND e.target_kind = 'Fact'
              AND e.relation = $2
             WHERE e.target_memory_id = $1",
        )
        .bind(commit_outcome.memory_id.into_inner())
        .bind(CORE_DERIVED_FROM_RELATION)
        .fetch_one(pg.pool())
        .await?;
        let run_memory_id: Uuid = run.try_get("memory_id")?;
        let target_branch: String = run.try_get("target_branch")?;
        let run_worktree_path: String = run.try_get("worktree_path")?;
        let parent_sha: String = run.try_get("parent_sha")?;
        let head_sha: String = run.try_get("head_sha")?;
        let diff_stat: serde_json::Value = run.try_get("diff_stat_json")?;
        assert_eq!(target_branch, "main");
        assert_eq!(parent_sha, commit_sha);
        assert_ne!(head_sha, parent_sha);
        assert_eq!(
            diff_stat
                .get("files_changed")
                .and_then(serde_json::Value::as_u64),
            Some(1)
        );
        let run_worktree = Path::new(&run_worktree_path);
        assert_eq!(git(run_worktree, &["status", "--porcelain"])?, "");
        let commit_message = git(run_worktree, &["log", "-1", "--format=%B"])?;
        assert!(
            commit_message.contains("proxima worker candidate"),
            "{commit_message}"
        );
        assert!(
            commit_message.contains(&format!(
                "Triggering-Memory: {}",
                commit_outcome.memory_id.into_inner()
            )),
            "{commit_message}"
        );
        assert!(
            commit_message.contains(&format!("Wake-Invocation: {invocation_id}")),
            "{commit_message}"
        );

        let authored_edges: i64 = sqlx::query_scalar(
            "SELECT count(*)
             FROM proxima_core.edges
             WHERE relation = $1
               AND source_kind = 'Perspective'
               AND source_memory_id = $2
               AND target_kind = 'Fact'
               AND target_memory_id = $3",
        )
        .bind(CORE_AUTHORED_RELATION)
        .bind(executor_root.into_inner())
        .bind(run_memory_id)
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(authored_edges, 1);

        let derived_edges: i64 = sqlx::query_scalar(
            "SELECT count(*)
             FROM proxima_core.edges
             WHERE relation = $1
               AND source_kind = 'Fact'
               AND source_memory_id = $2
               AND target_kind = 'Fact'
               AND target_memory_id = $3",
        )
        .bind(CORE_DERIVED_FROM_RELATION)
        .bind(run_memory_id)
        .bind(commit_outcome.memory_id.into_inner())
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(derived_edges, 1);

        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db_name).await;
    result
}
