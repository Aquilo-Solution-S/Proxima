#![allow(clippy::too_many_lines)]

use std::path::{Path, PathBuf};

use proxima_code::payloads::{WorkspaceDiffFile, WorkspaceDiffStat};
use proxima_code::{
    ExecutionRequestV1, WorkspaceDecision, WorkspaceReviewFinding, WorkspaceReviewV1,
    WorkspaceReviewVerdict, WorkspaceRunV1,
};
use proxima_core::verbs::event_ingest::{CitationMappingHint, CitedObjectHint, EventDraft};
use proxima_core::{
    CORE_DERIVED_FROM_RELATION, EdgeAuthorshipKind, EntityKind, FactPayload, FlavorRegistry,
    MemoryId, OrgId, Owner, Principal, SchemaId, SchemaVersion, SourceBatchId, SourceId, UserId,
};
use proxima_storage_pg::PgStorage;
use proxima_storage_pg::verbs::edge_append::{EdgeDraft, append_edge_in_tx};
use proxima_storage_pg::verbs::event_ingest::ingest_event_in_tx;
use sqlx::{Connection, Executor, PgConnection};
use tempfile::TempDir;
use uuid::Uuid;

const ADMIN_URL: &str = "postgres://proxima:proxima@localhost/postgres";
const WORKSPACE_REVIEW_SOURCE_ID: &str = "proxima-code/workspace-review";
const WORKSPACE_REVIEW_OBJECT_SCHEMA: &str = "proxima-code/workspace-review-object-v1";
const WORKSPACE_REVIEW_WHOLE_SCHEMA: &str = "proxima-code/workspace-review-whole-v1";
const EXECUTION_REQUEST_SOURCE_ID: &str = "proxima-code/execution-request";

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

fn worker_commit(
    repo_path: &Path,
    root: &TempDir,
    contents: &str,
) -> Result<(PathBuf, String, String), String> {
    let branch_name = format!("proxima/wake/{}", Uuid::now_v7());
    let worktree = root.path().join("worker");
    git(
        repo_path,
        &[
            "worktree",
            "add",
            "-b",
            &branch_name,
            worktree.to_str().expect("utf8 worktree"),
            "main",
        ],
    )?;
    std::fs::write(worktree.join("README.md"), contents).map_err(|err| err.to_string())?;
    git(&worktree, &["add", "README.md"])?;
    git(
        &worktree,
        &[
            "-c",
            "user.name=Proxima Test",
            "-c",
            "user.email=proxima@example.test",
            "commit",
            "-m",
            "worker change",
        ],
    )?;
    let head = git(&worktree, &["rev-parse", "HEAD"])?;
    Ok((worktree, branch_name, head))
}

#[tokio::test(flavor = "multi_thread")]
async fn merge_requires_approved_review() -> Result<(), Box<dyn std::error::Error>> {
    let Some((db_name, pg)) = migrated_db().await else {
        return Ok(());
    };
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = test_owner();
        let repo_root = tempfile::tempdir()?;
        let worker_root = tempfile::tempdir()?;
        let (repo_path, parent_sha) = init_repo(&repo_root).map_err(std::io::Error::other)?;
        let (worktree, branch_name, head_sha) =
            worker_commit(&repo_path, &worker_root, "initial\nchange\n")
                .map_err(std::io::Error::other)?;
        let repo_id = register_repo(&pg, &owner, &repo_path).await?;
        let (run, _) = seed_workspace_run(
            &pg,
            &owner,
            repo_id,
            &worktree,
            &branch_name,
            &parent_sha,
            &head_sha,
        )
        .await?;

        let err = proxima_code::merge_workspace_run(pg.pool(), &owner, run)
            .await
            .expect_err("merge should require approved review");
        assert!(matches!(
            err,
            proxima_code::WorkspaceFlowError::ApprovedReviewRequired { .. }
        ));
        assert_eq!(git(&repo_path, &["rev-parse", "main"])?, parent_sha);
        Ok(())
    }
    .await;
    drop(pg);
    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test(flavor = "multi_thread")]
async fn merge_fast_forwards_and_emits_decision() -> Result<(), Box<dyn std::error::Error>> {
    let Some((db_name, pg)) = migrated_db().await else {
        return Ok(());
    };
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = test_owner();
        let repo_root = tempfile::tempdir()?;
        let worker_root = tempfile::tempdir()?;
        let (repo_path, parent_sha) = init_repo(&repo_root).map_err(std::io::Error::other)?;
        let (worktree, branch_name, head_sha) =
            worker_commit(&repo_path, &worker_root, "initial\naccepted change\n")
                .map_err(std::io::Error::other)?;
        let repo_id = register_repo(&pg, &owner, &repo_path).await?;
        let (run, request) = seed_workspace_run(
            &pg,
            &owner,
            repo_id,
            &worktree,
            &branch_name,
            &parent_sha,
            &head_sha,
        )
        .await?;
        seed_workspace_review(&pg, &owner, run, request, WorkspaceReviewVerdict::Approved).await?;

        let outcome = proxima_code::merge_workspace_run(pg.pool(), &owner, run).await?;

        assert_eq!(outcome.run_memory_id, run.into_inner());
        assert_eq!(outcome.repo_id, repo_id);
        assert_eq!(outcome.target_branch, "main");
        assert_eq!(outcome.old_target_sha, parent_sha);
        assert_eq!(outcome.new_target_sha, head_sha);
        assert_eq!(git(&repo_path, &["rev-parse", "main"])?, head_sha);
        let decision: WorkspaceDecision = sqlx::query_scalar(
            "SELECT decision
             FROM proxima_code.workspace_decision_v1
             WHERE memory_id = $1",
        )
        .bind(outcome.decision_memory_id)
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(decision, WorkspaceDecision::Merged);
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
        .bind(outcome.decision_memory_id)
        .bind(run.into_inner())
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(decides_edges, 1);
        let runs = proxima_code::list_workspace_runs(pg.pool(), &owner, repo_id, 10).await?;
        assert_eq!(runs.len(), 1);
        assert_eq!(
            runs[0].execution_request_title.as_deref(),
            Some("Workspace flow request")
        );
        assert_eq!(
            runs[0].latest_decision.as_ref().map(|row| row.decision),
            Some(WorkspaceDecision::Merged)
        );
        Ok(())
    }
    .await;
    drop(pg);
    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test(flavor = "multi_thread")]
async fn rejected_decision_is_persisted() -> Result<(), Box<dyn std::error::Error>> {
    let Some((db_name, pg)) = migrated_db().await else {
        return Ok(());
    };
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = test_owner();
        let repo_root = tempfile::tempdir()?;
        let worker_root = tempfile::tempdir()?;
        let (repo_path, parent_sha) = init_repo(&repo_root).map_err(std::io::Error::other)?;
        let (worktree, branch_name, head_sha) =
            worker_commit(&repo_path, &worker_root, "initial\nrejected change\n")
                .map_err(std::io::Error::other)?;
        let repo_id = register_repo(&pg, &owner, &repo_path).await?;
        let (run, _) = seed_workspace_run(
            &pg,
            &owner,
            repo_id,
            &worktree,
            &branch_name,
            &parent_sha,
            &head_sha,
        )
        .await?;

        let decision = proxima_code::emit_workspace_decision(
            pg.pool(),
            &owner,
            run,
            WorkspaceDecision::Rejected,
            Some("wrong behavior"),
        )
        .await?;

        let row: (WorkspaceDecision, Option<String>) = sqlx::query_as(
            "SELECT decision, reason_text
             FROM proxima_code.workspace_decision_v1
             WHERE memory_id = $1",
        )
        .bind(decision.into_inner())
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(row.0, WorkspaceDecision::Rejected);
        assert_eq!(row.1.as_deref(), Some("wrong behavior"));
        let runs = proxima_code::list_workspace_runs(pg.pool(), &owner, repo_id, 10).await?;
        assert_eq!(
            runs[0].latest_decision.as_ref().map(|row| row.decision),
            Some(WorkspaceDecision::Rejected)
        );
        Ok(())
    }
    .await;
    drop(pg);
    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test(flavor = "multi_thread")]
async fn retry_requested_decision_is_persisted() -> Result<(), Box<dyn std::error::Error>> {
    let Some((db_name, pg)) = migrated_db().await else {
        return Ok(());
    };
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = test_owner();
        let repo_root = tempfile::tempdir()?;
        let worker_root = tempfile::tempdir()?;
        let (repo_path, parent_sha) = init_repo(&repo_root).map_err(std::io::Error::other)?;
        let (worktree, branch_name, head_sha) =
            worker_commit(&repo_path, &worker_root, "initial\nretry change\n")
                .map_err(std::io::Error::other)?;
        let repo_id = register_repo(&pg, &owner, &repo_path).await?;
        let (run, _) = seed_workspace_run(
            &pg,
            &owner,
            repo_id,
            &worktree,
            &branch_name,
            &parent_sha,
            &head_sha,
        )
        .await?;

        let decision = proxima_code::emit_workspace_decision(
            pg.pool(),
            &owner,
            run,
            WorkspaceDecision::RetryRequested,
            Some("try again"),
        )
        .await?;

        let row: (WorkspaceDecision, Option<String>) = sqlx::query_as(
            "SELECT decision, reason_text
             FROM proxima_code.workspace_decision_v1
             WHERE memory_id = $1",
        )
        .bind(decision.into_inner())
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(row.0, WorkspaceDecision::RetryRequested);
        assert_eq!(row.1.as_deref(), Some("try again"));
        let runs = proxima_code::list_workspace_runs(pg.pool(), &owner, repo_id, 10).await?;
        assert_eq!(
            runs[0].latest_decision.as_ref().map(|row| row.decision),
            Some(WorkspaceDecision::RetryRequested)
        );
        Ok(())
    }
    .await;
    drop(pg);
    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test(flavor = "multi_thread")]
async fn merge_rejects_latest_decision_after_approval() -> Result<(), Box<dyn std::error::Error>> {
    let Some((db_name, pg)) = migrated_db().await else {
        return Ok(());
    };
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = test_owner();
        let repo_root = tempfile::tempdir()?;
        let worker_root = tempfile::tempdir()?;
        let (repo_path, parent_sha) = init_repo(&repo_root).map_err(std::io::Error::other)?;
        let (worktree, branch_name, head_sha) =
            worker_commit(&repo_path, &worker_root, "initial\napproved change\n")
                .map_err(std::io::Error::other)?;
        let repo_id = register_repo(&pg, &owner, &repo_path).await?;
        let (run, request) = seed_workspace_run(
            &pg,
            &owner,
            repo_id,
            &worktree,
            &branch_name,
            &parent_sha,
            &head_sha,
        )
        .await?;
        seed_workspace_review(&pg, &owner, run, request, WorkspaceReviewVerdict::Approved).await?;
        proxima_code::emit_workspace_decision(
            pg.pool(),
            &owner,
            run,
            WorkspaceDecision::RetryRequested,
            Some("needs another pass"),
        )
        .await?;

        let err = proxima_code::merge_workspace_run(pg.pool(), &owner, run)
            .await
            .expect_err("merge should reject latest terminal decisions");
        assert!(matches!(
            err,
            proxima_code::WorkspaceFlowError::LaterWorkspaceDecision { .. }
        ));
        assert_eq!(git(&repo_path, &["rev-parse", "main"])?, parent_sha);
        Ok(())
    }
    .await;
    drop(pg);
    let _ = drop_db(&db_name).await;
    result
}

async fn register_repo(
    pg: &PgStorage,
    owner: &Owner,
    repo_path: &Path,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let repo_id = Uuid::now_v7();
    proxima_code::register_repo(
        pg.pool(),
        owner,
        repo_id,
        repo_path.to_str().expect("utf8 repo path"),
        "workspace-flow",
    )
    .await?;
    Ok(repo_id)
}

async fn seed_workspace_run(
    pg: &PgStorage,
    owner: &Owner,
    repo_id: Uuid,
    worktree: &Path,
    branch_name: &str,
    parent_sha: &str,
    head_sha: &str,
) -> Result<(MemoryId, MemoryId), Box<dyn std::error::Error>> {
    let request = seed_execution_request(pg, owner, repo_id).await?;
    let payload = WorkspaceRunV1 {
        wake_invocation_id: Uuid::now_v7(),
        repo_id,
        target_branch: "main".into(),
        worktree_path: worktree.to_string_lossy().to_string(),
        branch_name: branch_name.into(),
        parent_sha: parent_sha.into(),
        head_sha: head_sha.into(),
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
        stdout_tail: Some("ok".into()),
        stderr_tail: None,
        duration_ms: Some(10),
    };
    let mut payload_bytes = Vec::new();
    ciborium::ser::into_writer(&payload, &mut payload_bytes)?;
    let content_hash = blake3::hash(&payload_bytes);
    let observed_at = time::OffsetDateTime::now_utc();
    let draft = EventDraft {
        source_id: SourceId::new(proxima_code::WORKSPACE_RUNNER_SOURCE_ID),
        source_batch_id: SourceBatchId::new(Uuid::now_v7()),
        owner: owner.clone(),
        schema_id: SchemaId::new(WorkspaceRunV1::SCHEMA_ID.into()),
        schema_version: SchemaVersion::new(WorkspaceRunV1::SCHEMA_VERSION),
        payload: payload_bytes,
        observed_at,
        occurred_at: observed_at,
        cited_object: CitedObjectHint {
            schema_id: SchemaId::new(proxima_code::WORKSPACE_RUN_OBJECT_SCHEMA.into()),
            schema_version: SchemaVersion::new(1),
            content_hash: *content_hash.as_bytes(),
        },
        citation_mapping: CitationMappingHint {
            schema_id: SchemaId::new(proxima_code::WORKSPACE_RUN_WHOLE_SCHEMA.into()),
            schema_version: SchemaVersion::new(1),
        },
    };
    let mut tx = pg.pool().begin().await?;
    let outcome = ingest_event_in_tx(&mut tx, &draft).await?;
    sqlx::query(
        "INSERT INTO proxima_code.workspace_run_v1
            (memory_id, wake_invocation_id, repo_id, target_branch, worktree_path,
             branch_name, parent_sha, head_sha, diff_stat_json, exit_code,
             stdout_tail, stderr_tail, duration_ms)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)",
    )
    .bind(outcome.memory_id.into_inner())
    .bind(payload.wake_invocation_id)
    .bind(repo_id)
    .bind(&payload.target_branch)
    .bind(&payload.worktree_path)
    .bind(&payload.branch_name)
    .bind(&payload.parent_sha)
    .bind(&payload.head_sha)
    .bind(serde_json::to_value(&payload.diff_stat_json)?)
    .bind(payload.exit_code)
    .bind(payload.stdout_tail.as_deref())
    .bind(payload.stderr_tail.as_deref())
    .bind(
        payload
            .duration_ms
            .and_then(|value| i64::try_from(value).ok()),
    )
    .execute(&mut *tx)
    .await?;
    let mut registry = FlavorRegistry::new();
    proxima_code::register(&mut registry);
    let registry = registry.freeze();
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
    Ok((outcome.memory_id, request))
}

async fn seed_execution_request(
    pg: &PgStorage,
    owner: &Owner,
    repo_id: Uuid,
) -> Result<MemoryId, Box<dyn std::error::Error>> {
    let payload = ExecutionRequestV1 {
        repo_id,
        title: "Workspace flow request".into(),
        instructions: "Make the workspace flow change.".into(),
        request_key: format!("workspace-flow-{}", Uuid::now_v7()),
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
            schema_id: SchemaId::new(proxima_code::EXECUTION_REQUEST_OBJECT_SCHEMA.into()),
            schema_version: SchemaVersion::new(1),
            content_hash: *content_hash.as_bytes(),
        },
        citation_mapping: CitationMappingHint {
            schema_id: SchemaId::new(proxima_code::EXECUTION_REQUEST_WHOLE_SCHEMA.into()),
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
    .bind(&payload.title)
    .bind(&payload.instructions)
    .bind(&payload.request_key)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(outcome.memory_id)
}

async fn seed_workspace_review(
    pg: &PgStorage,
    owner: &Owner,
    run: MemoryId,
    request: MemoryId,
    verdict: WorkspaceReviewVerdict,
) -> Result<MemoryId, Box<dyn std::error::Error>> {
    let payload = WorkspaceReviewV1 {
        workspace_run_memory_id: run.into_inner(),
        execution_request_memory_id: request.into_inner(),
        verdict,
        round_index: 0,
        summary: "review summary".into(),
        findings: vec![WorkspaceReviewFinding {
            severity: "info".into(),
            file_path: Some("README.md".into()),
            line: Some(1),
            message: "ok".into(),
        }],
        correction_instructions: None,
        verification_summary: Some("passed".into()),
        reviewed_at: time::OffsetDateTime::now_utc(),
    };
    let mut payload_bytes = Vec::new();
    ciborium::ser::into_writer(&payload, &mut payload_bytes)?;
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
