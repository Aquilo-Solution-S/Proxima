#![allow(clippy::too_many_lines)]

use std::sync::Arc;

use proxima_code::mcp::{CodeEmitCorrectionExecutionRequestTool, CodeEmitWorkspaceReviewTool};
use proxima_code::payloads::WorkspaceDiffStat;
use proxima_code::{
    CommitV1, ExecutionRequestV1, WorkspaceDecision, WorkspaceReviewVerdict, WorkspaceRunV1,
};
use proxima_core::auth::NoAuth;
use proxima_core::mcp::{HandleTable, McpAuthorContext, McpTool, McpToolCtx};
use proxima_core::personality::{
    InstantiatePersonalityRequest, SetWakeEntriesRequest, WakeEntryDraft, WakeExecutionMode,
};
use proxima_core::storage::Storage;
use proxima_core::verbs::event_ingest::{CitationMappingHint, CitedObjectHint, EventDraft};
use proxima_core::verbs::query::MemoryStore;
use proxima_core::verbs::schema::{PayloadKind, SchemaInfo};
use proxima_core::{
    BindInferenceTierRequest, CORE_AUTHORED_RELATION, CORE_DERIVED_FROM_RELATION, FactPayload,
    FlavorRegistry, FlavorRegistryFrozen, InferenceTargetConfig, LocalCliConfig, MemoryId,
    ModelTier, OrgId, Owner, Principal, RegisterInferenceTargetRequest, SchemaId, SchemaVersion,
    SourceBatchId, SourceId, UserId, WakeEntryAuthoredBy, WakeEntryTriggerKind,
};
use proxima_storage_pg::PgStorage;
use proxima_storage_pg::verbs::edge_append::{EdgeDraft, append_edge_in_tx};
use proxima_storage_pg::verbs::event_ingest::ingest_event_in_tx;
use serde_json::json;
use sqlx::{Connection, Executor, PgConnection, Row};
use uuid::Uuid;

const ADMIN_URL: &str = "postgres://proxima:proxima@localhost/postgres";
const EXECUTION_REQUEST_SOURCE_ID: &str = "proxima-code/execution-request";
const EXECUTION_REQUEST_OBJECT_SCHEMA: &str = "proxima-code/execution-request-object-v1";
const EXECUTION_REQUEST_WHOLE_SCHEMA: &str = "proxima-code/execution-request-whole-v1";
const WORKSPACE_RUNNER_SOURCE_ID: &str = "proxima-code/workspace-runner";
const WORKSPACE_RUN_OBJECT_SCHEMA: &str = "proxima-code/workspace-run-object-v1";
const WORKSPACE_RUN_WHOLE_SCHEMA: &str = "proxima-code/workspace-run-whole-v1";

#[tokio::test]
async fn emit_workspace_review_writes_review_and_edges() -> Result<(), Box<dyn std::error::Error>> {
    let Some(fixture) = TestDb::fresh().await? else {
        return Ok(());
    };
    let owner = owner_fixture();
    let registry = registry_for_mcp();
    let root = root_perspective(&fixture.pg, &owner).await?;
    let repo_id = Uuid::now_v7();
    let request = seed_execution_request(
        &fixture.pg,
        &owner,
        registry.as_ref(),
        repo_id,
        "review-request",
        "Review request",
        "Change the workspace code.",
        &[],
    )
    .await?;
    let run = seed_workspace_run(&fixture.pg, &owner, registry.as_ref(), repo_id, request).await?;

    let output = run_tool::<CodeEmitWorkspaceReviewTool>(
        ctx(
            fixture.pg.pool().clone(),
            owner.clone(),
            registry.clone(),
            Some(root),
            None,
        ),
        json!({
            "workspace_run_memory": run.into_inner().to_string(),
            "verdict": "approved",
            "summary": "Implementation matches the request.",
            "findings": [],
            "verification_summary": "targeted checks passed",
            "idempotency_key": "review-1"
        }),
    )
    .await?;

    assert_eq!(output["verdict"], "approved");
    assert_eq!(output["round_index"], 0);
    assert!(
        output["handle"].as_str().is_some(),
        "review output must include a memory handle"
    );
    let review_memory: Uuid = sqlx::query_scalar(
        "SELECT memory_id
         FROM proxima_code.workspace_review_v1
         WHERE workspace_run_memory_id = $1
           AND execution_request_memory_id = $2
         ORDER BY created_at DESC
         LIMIT 1",
    )
    .bind(run.into_inner())
    .bind(request.into_inner())
    .fetch_one(fixture.pg.pool())
    .await?;
    let review = sqlx::query(
        "SELECT verdict, round_index, summary, verification_summary
         FROM proxima_code.workspace_review_v1
         WHERE memory_id = $1",
    )
    .bind(review_memory)
    .fetch_one(fixture.pg.pool())
    .await?;
    assert_eq!(review.try_get::<String, _>("verdict")?, "approved");
    assert_eq!(review.try_get::<i32, _>("round_index")?, 0);
    assert_eq!(
        review.try_get::<String, _>("summary")?,
        "Implementation matches the request."
    );
    assert_eq!(
        review.try_get::<Option<String>, _>("verification_summary")?,
        Some("targeted checks passed".into())
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
    .bind(root.into_inner())
    .bind(review_memory)
    .fetch_one(fixture.pg.pool())
    .await?;
    assert_eq!(authored_edges, 1);

    for target in [run.into_inner(), request.into_inner()] {
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
        .bind(review_memory)
        .bind(target)
        .fetch_one(fixture.pg.pool())
        .await?;
        assert_eq!(derived_edges, 1, "missing derived edge to {target}");
    }

    Ok(())
}

#[tokio::test]
async fn rejected_review_at_veto_limit_becomes_needs_user() -> Result<(), Box<dyn std::error::Error>>
{
    let Some(fixture) = TestDb::fresh().await? else {
        return Ok(());
    };
    let owner = owner_fixture();
    let registry = registry_for_mcp();
    let root = root_perspective(&fixture.pg, &owner).await?;
    let repo_id = Uuid::now_v7();
    let request = seed_execution_request(
        &fixture.pg,
        &owner,
        registry.as_ref(),
        repo_id,
        "limit-request",
        "Limit request",
        "Make the change.",
        &[],
    )
    .await?;
    let run = seed_workspace_run(&fixture.pg, &owner, registry.as_ref(), repo_id, request).await?;
    for idx in 0..2 {
        seed_workspace_review(
            &fixture.pg,
            &owner,
            registry.clone(),
            run,
            request,
            WorkspaceReviewVerdict::Rejected,
            idx,
        )
        .await?;
    }

    let output = run_tool::<CodeEmitWorkspaceReviewTool>(
        ctx(
            fixture.pg.pool().clone(),
            owner.clone(),
            registry.clone(),
            Some(root),
            None,
        ),
        json!({
            "workspace_run_memory": run.into_inner().to_string(),
            "verdict": "rejected",
            "summary": "Still incomplete.",
            "findings": [{ "severity": "major", "file_path": "src/lib.rs", "line": 12, "message": "Missing behavior." }],
            "correction_instructions": "Escalate instead of retrying.",
            "idempotency_key": "review-limit"
        }),
    )
    .await?;

    assert_eq!(output["verdict"], "needs_user");
    assert_eq!(output["round_index"], 2);
    let needs_user_rows: i64 = sqlx::query_scalar(
        "SELECT count(*)
         FROM proxima_code.workspace_review_v1
         WHERE execution_request_memory_id = $1
           AND verdict = 'needs_user'",
    )
    .bind(request.into_inner())
    .fetch_one(fixture.pg.pool())
    .await?;
    assert_eq!(needs_user_rows, 1);
    Ok(())
}

#[tokio::test]
async fn correction_request_derives_from_review_and_targets_worker()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(fixture) = TestDb::fresh().await? else {
        return Ok(());
    };
    let owner = owner_fixture();
    let registry = registry_for_mcp();
    let root = root_perspective(&fixture.pg, &owner).await?;
    let repo_id = Uuid::now_v7();
    let evidence = seed_commit(&fixture.pg, &owner, repo_id).await?;
    let request = seed_execution_request(
        &fixture.pg,
        &owner,
        registry.as_ref(),
        repo_id,
        "correction-request",
        "Original implementation",
        "Implement the original change.",
        &[evidence],
    )
    .await?;
    let run = seed_workspace_run(&fixture.pg, &owner, registry.as_ref(), repo_id, request).await?;
    let review = seed_workspace_review(
        &fixture.pg,
        &owner,
        registry.clone(),
        run,
        request,
        WorkspaceReviewVerdict::Rejected,
        0,
    )
    .await?;

    let engine = engine_for_test(fixture.pg.clone(), owner.clone());
    let worker = engine
        .instantiate_personality(InstantiatePersonalityRequest {
            owner: owner.clone(),
            display_name: "Correction Worker".into(),
            purpose: "Executes correction requests".into(),
        })
        .await?;
    configure_execution_worker(&fixture.pg, &owner, worker.instance_id).await?;
    let worker_runtime = fixture
        .pg
        .fetch_personality_runtime(&owner, worker.instance_id)
        .await?
        .expect("worker runtime");

    let output = run_tool::<CodeEmitCorrectionExecutionRequestTool>(
        ctx(
            fixture.pg.pool().clone(),
            owner.clone(),
            registry.clone(),
            Some(root),
            Some(Uuid::now_v7()),
        ),
        json!({
            "workspace_review_memory": review.into_inner().to_string(),
            "target_personality": worker.instance_id.into_inner().to_string(),
            "request_key": "correction-request:correction:0",
            "idempotency_key": "correction-request:correction:0"
        }),
    )
    .await?;

    assert_eq!(output["idempotent_replay"], false);
    assert!(
        output["target_edge_handle"].as_str().is_some(),
        "correction request must target worker"
    );
    let row = sqlx::query(
        "SELECT memory_id, title, instructions
         FROM proxima_code.execution_request_v1
         WHERE repo_id = $1 AND request_key = 'correction-request:correction:0'",
    )
    .bind(repo_id)
    .fetch_one(fixture.pg.pool())
    .await?;
    let correction_memory: Uuid = row.try_get("memory_id")?;
    assert_eq!(
        row.try_get::<String, _>("title")?,
        "Correct: Original implementation"
    );
    let instructions: String = row.try_get("instructions")?;
    assert!(instructions.contains("Implement the original change."));
    assert!(instructions.contains(&review.into_inner().to_string()));
    assert!(instructions.contains("Fix the incomplete implementation."));

    let target_edges: i64 = sqlx::query_scalar(
        "SELECT count(*)
         FROM proxima_core.edges
         WHERE relation = $1
           AND source_kind = 'Perspective'
           AND source_memory_id = $2
           AND target_kind = 'Fact'
           AND target_memory_id = $3",
    )
    .bind(proxima_code::mcp::CODE_TARGETS_EXECUTION_REQUEST_RELATION)
    .bind(
        worker_runtime
            .current_root_perspective_memory_id
            .into_inner(),
    )
    .bind(correction_memory)
    .fetch_one(fixture.pg.pool())
    .await?;
    assert_eq!(target_edges, 1);

    for target in [
        review.into_inner(),
        run.into_inner(),
        request.into_inner(),
        evidence.into_inner(),
    ] {
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
        .bind(correction_memory)
        .bind(target)
        .fetch_one(fixture.pg.pool())
        .await?;
        assert_eq!(derived_edges, 1, "missing derived edge to {target}");
    }

    let replay = run_tool::<CodeEmitCorrectionExecutionRequestTool>(
        ctx(
            fixture.pg.pool().clone(),
            owner,
            registry,
            Some(root),
            Some(Uuid::now_v7()),
        ),
        json!({
            "workspace_review_memory": review.into_inner().to_string(),
            "target_personality": Uuid::now_v7().to_string(),
            "request_key": "correction-request:correction:0",
            "idempotency_key": "correction-request:correction:0"
        }),
    )
    .await?;
    assert_eq!(replay["idempotent_replay"], true);
    Ok(())
}

#[tokio::test]
async fn correction_request_can_derive_from_retry_decision_without_rejected_review()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(fixture) = TestDb::fresh().await? else {
        return Ok(());
    };
    let owner = owner_fixture();
    let registry = registry_for_mcp();
    let root = root_perspective(&fixture.pg, &owner).await?;
    let repo_id = Uuid::now_v7();
    let request = seed_execution_request(
        &fixture.pg,
        &owner,
        registry.as_ref(),
        repo_id,
        "retry-decision-request",
        "Original implementation",
        "Implement the original change.",
        &[],
    )
    .await?;
    let run = seed_workspace_run(&fixture.pg, &owner, registry.as_ref(), repo_id, request).await?;
    let decision = proxima_code::emit_workspace_decision(
        fixture.pg.pool(),
        &owner,
        run,
        WorkspaceDecision::RetryRequested,
        Some("Please try a narrower correction."),
    )
    .await?;

    let engine = engine_for_test(fixture.pg.clone(), owner.clone());
    let worker = engine
        .instantiate_personality(InstantiatePersonalityRequest {
            owner: owner.clone(),
            display_name: "Retry Worker".into(),
            purpose: "Executes retry requests".into(),
        })
        .await?;
    configure_execution_worker(&fixture.pg, &owner, worker.instance_id).await?;

    let output = run_tool::<CodeEmitCorrectionExecutionRequestTool>(
        ctx(
            fixture.pg.pool().clone(),
            owner.clone(),
            registry,
            Some(root),
            Some(Uuid::now_v7()),
        ),
        json!({
            "workspace_decision_memory": decision.into_inner().to_string(),
            "target_personality": worker.instance_id.into_inner().to_string(),
            "request_key": "retry-decision-request:correction:0",
            "idempotency_key": "retry-decision-request:correction:0"
        }),
    )
    .await?;

    assert_eq!(output["idempotent_replay"], false);
    let row = sqlx::query(
        "SELECT memory_id, instructions
         FROM proxima_code.execution_request_v1
         WHERE repo_id = $1 AND request_key = 'retry-decision-request:correction:0'",
    )
    .bind(repo_id)
    .fetch_one(fixture.pg.pool())
    .await?;
    let correction_memory: Uuid = row.try_get("memory_id")?;
    let instructions: String = row.try_get("instructions")?;
    assert!(instructions.contains(&decision.into_inner().to_string()));
    assert!(instructions.contains("Please try a narrower correction."));
    assert!(instructions.contains("workspace_review: none"));

    for target in [
        decision.into_inner(),
        run.into_inner(),
        request.into_inner(),
    ] {
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
        .bind(correction_memory)
        .bind(target)
        .fetch_one(fixture.pg.pool())
        .await?;
        assert_eq!(derived_edges, 1, "missing derived edge to {target}");
    }

    Ok(())
}

#[tokio::test]
async fn emit_workspace_review_resolves_request_through_run_chain()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(fixture) = TestDb::fresh().await? else {
        return Ok(());
    };
    let owner = owner_fixture();
    let registry = registry_for_mcp();
    let root = root_perspective(&fixture.pg, &owner).await?;
    let repo_id = Uuid::now_v7();
    let request = seed_execution_request(
        &fixture.pg,
        &owner,
        registry.as_ref(),
        repo_id,
        "chained-review-request",
        "Original implementation",
        "Implement the original change.",
        &[],
    )
    .await?;
    let first_run =
        seed_workspace_run(&fixture.pg, &owner, registry.as_ref(), repo_id, request).await?;
    let chained_run =
        seed_workspace_run(&fixture.pg, &owner, registry.as_ref(), repo_id, first_run).await?;

    let output = run_tool::<CodeEmitWorkspaceReviewTool>(
        ctx(
            fixture.pg.pool().clone(),
            owner.clone(),
            registry,
            Some(root),
            None,
        ),
        json!({
            "workspace_run_memory": chained_run.into_inner().to_string(),
            "verdict": "approved",
            "summary": "Chained run satisfies the request.",
            "findings": [],
            "verification_summary": "targeted checks passed",
            "idempotency_key": "chained-review"
        }),
    )
    .await?;

    assert_eq!(output["verdict"], "approved");
    let review_request: Uuid = sqlx::query_scalar(
        "SELECT execution_request_memory_id
         FROM proxima_code.workspace_review_v1
         WHERE workspace_run_memory_id = $1
         ORDER BY created_at DESC
         LIMIT 1",
    )
    .bind(chained_run.into_inner())
    .fetch_one(fixture.pg.pool())
    .await?;
    assert_eq!(review_request, request.into_inner());
    Ok(())
}

async fn run_tool<T: McpTool>(
    ctx: McpToolCtx,
    args: serde_json::Value,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let typed: T::Args = serde_json::from_value(args)?;
    let output = T::call(ctx, typed).await?;
    Ok(serde_json::to_value(output)?)
}

fn ctx(
    pool: sqlx::PgPool,
    owner: Owner,
    registry: Arc<FlavorRegistryFrozen>,
    caller_self_perspective: Option<MemoryId>,
    master_token_id: Option<Uuid>,
) -> McpToolCtx {
    McpToolCtx {
        pool,
        owner,
        handles: Arc::new(HandleTable::new()),
        registry,
        author: McpAuthorContext {
            model_id: "test/0".into(),
            client_name: "test".into(),
            client_version: "0".into(),
            caller_self_perspective,
        },
        caller_self_perspective,
        master_token_id,
        engine: None,
    }
}

#[derive(Debug)]
struct TestDb {
    name: String,
    pg: PgStorage,
}

impl TestDb {
    async fn fresh() -> Result<Option<Self>, Box<dyn std::error::Error>> {
        let name = format!("proxima_test_{}", Uuid::now_v7().simple());
        if create_db(&name).await.is_err() {
            eprintln!("skipping (no admin PG)");
            return Ok(None);
        }
        let setup: Result<PgStorage, Box<dyn std::error::Error>> = async {
            let pg = PgStorage::connect(&db_url(&name)).await?;
            pg.run_migrations().await?;
            proxima_code::migrator().run(pg.pool()).await?;
            Ok(pg)
        }
        .await;
        match setup {
            Ok(pg) => Ok(Some(Self { name, pg })),
            Err(error) => {
                let _ = drop_db(&name).await;
                Err(error)
            }
        }
    }
}

impl Drop for TestDb {
    fn drop(&mut self) {
        let name = self.name.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Runtime::new().expect("drop runtime");
            runtime.block_on(async {
                let _ = drop_db(&name).await;
            });
        })
        .join()
        .expect("drop db thread");
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

fn owner_fixture() -> Owner {
    Owner {
        principal: Principal::User(UserId::new(Uuid::now_v7())),
        org_id: OrgId::new(Uuid::now_v7()),
    }
}

fn registry_for_mcp() -> Arc<FlavorRegistryFrozen> {
    let mut registry = FlavorRegistry::new();
    proxima_code::register(&mut registry);
    Arc::new(registry.freeze())
}

fn registry_for_engine() -> FlavorRegistryFrozen {
    let mut flavor = FlavorRegistry::new();
    proxima_code::register(&mut flavor);
    let mut schemas = flavor.freeze().list();
    schemas.push(SchemaInfo {
        schema_id: SchemaId::new("test/cited_blob".into()),
        schema_version: SchemaVersion::new(1),
        kind: PayloadKind::CitedObject,
        filter_keys: vec![],
        sidecar_table: None,
        natural_key_columns: vec![],
        tombstone: None,
        cbor_encoder: None,
    });
    schemas.push(SchemaInfo {
        schema_id: SchemaId::new("test/citation_blob".into()),
        schema_version: SchemaVersion::new(1),
        kind: PayloadKind::CitationMapping,
        filter_keys: vec![],
        sidecar_table: None,
        natural_key_columns: vec![],
        tombstone: None,
        cbor_encoder: None,
    });
    FlavorRegistryFrozen::with_schemas(schemas)
}

fn engine_for_test(pg: PgStorage, owner: Owner) -> proxima_core::Engine {
    let principal = owner.principal.clone();
    let storage: Arc<dyn Storage> = Arc::new(pg);
    proxima_core::Engine::new(
        registry_for_engine(),
        MemoryStore::new(),
        Box::new(NoAuth::new(principal, owner)),
    )
    .with_storage(storage)
}

async fn root_perspective(
    pg: &PgStorage,
    owner: &Owner,
) -> Result<MemoryId, Box<dyn std::error::Error>> {
    let engine = engine_for_test(pg.clone(), owner.clone());
    let instance = engine
        .instantiate_personality(InstantiatePersonalityRequest {
            owner: owner.clone(),
            display_name: "Review Author".into(),
            purpose: "Authors workspace reviews".into(),
        })
        .await?;
    let runtime = pg
        .fetch_personality_runtime(owner, instance.instance_id)
        .await?
        .expect("review author runtime");
    Ok(runtime.current_root_perspective_memory_id)
}

async fn seed_commit(
    pg: &PgStorage,
    owner: &Owner,
    repo_id: Uuid,
) -> Result<MemoryId, Box<dyn std::error::Error>> {
    let payload = CommitV1 {
        repo_id,
        sha: "abc123".into(),
        parents: Vec::new(),
        author_name: "Proxima Test".into(),
        author_email: "proxima@example.test".into(),
        author_time: time::OffsetDateTime::now_utc(),
        committer_name: "Proxima Test".into(),
        committer_email: "proxima@example.test".into(),
        committer_time: time::OffsetDateTime::now_utc(),
        message: "seed evidence".into(),
    };
    let outcome = proxima_code::ingest_commit(
        pg.pool(),
        owner,
        SourceBatchId::new(Uuid::now_v7()),
        &payload,
        time::OffsetDateTime::now_utc(),
    )
    .await?;
    Ok(outcome.memory_id)
}

#[allow(clippy::too_many_arguments)]
async fn seed_execution_request(
    pg: &PgStorage,
    owner: &Owner,
    registry: &FlavorRegistryFrozen,
    repo_id: Uuid,
    request_key: &str,
    title: &str,
    instructions: &str,
    evidence: &[MemoryId],
) -> Result<MemoryId, Box<dyn std::error::Error>> {
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
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(outcome.memory_id.into_inner())
    .bind(repo_id)
    .bind(title)
    .bind(instructions)
    .bind(request_key)
    .execute(&mut *tx)
    .await?;
    let relation = registry
        .resolve_relation(CORE_DERIVED_FROM_RELATION)
        .expect("core/derived-from registered");
    for target in evidence {
        append_edge_in_tx(
            &mut tx,
            &EdgeDraft {
                edge_id: Uuid::now_v7(),
                relation,
                source_kind: "Fact",
                source_memory_id: Some(outcome.memory_id.into_inner()),
                source_goal_id: None,
                target_kind: "Fact",
                target_memory_id: Some(target.into_inner()),
                target_goal_id: None,
                authorship_kind: "ExternalAgent",
                authorship_owner_memory_id: None,
                owner,
            },
            None,
        )
        .await?;
    }
    tx.commit().await?;
    Ok(outcome.memory_id)
}

async fn seed_workspace_run(
    pg: &PgStorage,
    owner: &Owner,
    registry: &FlavorRegistryFrozen,
    repo_id: Uuid,
    request: MemoryId,
) -> Result<MemoryId, Box<dyn std::error::Error>> {
    let payload = WorkspaceRunV1 {
        wake_invocation_id: Uuid::now_v7(),
        repo_id,
        target_branch: "main".into(),
        worktree_path: "/tmp/proxima-review-test".into(),
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
    };
    let mut payload_bytes = Vec::new();
    ciborium::ser::into_writer(&payload, &mut payload_bytes)?;
    let content_hash = blake3::hash(&payload_bytes);
    let observed_at = time::OffsetDateTime::now_utc();
    let draft = EventDraft {
        source_id: SourceId::new(WORKSPACE_RUNNER_SOURCE_ID),
        source_batch_id: SourceBatchId::new(Uuid::now_v7()),
        owner: owner.clone(),
        schema_id: SchemaId::new(WorkspaceRunV1::SCHEMA_ID.into()),
        schema_version: SchemaVersion::new(WorkspaceRunV1::SCHEMA_VERSION),
        payload: payload_bytes,
        observed_at,
        occurred_at: observed_at,
        cited_object: CitedObjectHint {
            schema_id: SchemaId::new(WORKSPACE_RUN_OBJECT_SCHEMA.into()),
            schema_version: SchemaVersion::new(1),
            content_hash: *content_hash.as_bytes(),
        },
        citation_mapping: CitationMappingHint {
            schema_id: SchemaId::new(WORKSPACE_RUN_WHOLE_SCHEMA.into()),
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
    .bind(payload.duration_ms.and_then(|v| i64::try_from(v).ok()))
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
            source_kind: "Fact",
            source_memory_id: Some(outcome.memory_id.into_inner()),
            source_goal_id: None,
            target_kind: "Fact",
            target_memory_id: Some(request.into_inner()),
            target_goal_id: None,
            authorship_kind: "EventSource",
            authorship_owner_memory_id: None,
            owner,
        },
        None,
    )
    .await?;
    tx.commit().await?;
    Ok(outcome.memory_id)
}

async fn seed_workspace_review(
    pg: &PgStorage,
    owner: &Owner,
    registry: Arc<FlavorRegistryFrozen>,
    run: MemoryId,
    request: MemoryId,
    verdict: WorkspaceReviewVerdict,
    round_index: u32,
) -> Result<MemoryId, Box<dyn std::error::Error>> {
    let root = root_perspective(pg, owner).await?;
    let output = run_tool::<CodeEmitWorkspaceReviewTool>(
        ctx(
            pg.pool().clone(),
            owner.clone(),
            registry.clone(),
            Some(root),
            None,
        ),
        json!({
            "workspace_run_memory": run.into_inner().to_string(),
            "verdict": verdict,
            "summary": "Implementation is incomplete.",
            "findings": [{ "severity": "major", "file_path": "src/lib.rs", "line": 1, "message": "Missing requested behavior." }],
            "correction_instructions": "Fix the incomplete implementation.",
            "verification_summary": "verification failed",
            "idempotency_key": format!("seed-review-{round_index}")
        }),
    )
    .await?;
    let review: Uuid = sqlx::query_scalar(
        "SELECT memory_id
         FROM proxima_code.workspace_review_v1
         WHERE workspace_run_memory_id = $1
           AND execution_request_memory_id = $2
         ORDER BY created_at DESC
         LIMIT 1",
    )
    .bind(run.into_inner())
    .bind(request.into_inner())
    .fetch_one(pg.pool())
    .await?;
    assert_eq!(output["round_index"], round_index);
    Ok(MemoryId::new(review))
}

async fn configure_execution_worker(
    pg: &PgStorage,
    owner: &Owner,
    worker: proxima_core::PersonalityInstanceId,
) -> Result<(), Box<dyn std::error::Error>> {
    pg.register_inference_target(&RegisterInferenceTargetRequest {
        owner: owner.clone(),
        target_ref: "test/correction-worker".into(),
        config: InferenceTargetConfig::LocalCli(LocalCliConfig {
            command: "correction-worker".into(),
            profile: None,
            env_overrides: Vec::new(),
        }),
    })
    .await?;
    pg.bind_inference_tier(&BindInferenceTierRequest {
        owner: owner.clone(),
        tier: ModelTier::Standard,
        target_ref: "test/correction-worker".into(),
    })
    .await?;
    let mut wake_entry = WakeEntryDraft::new(
        Uuid::now_v7(),
        worker,
        WakeEntryTriggerKind::OnMemory,
        ExecutionRequestV1::SCHEMA_ID,
        "execution-worker",
        WakeEntryAuthoredBy::Any,
        1000,
        "bundled:proxima-code/execution_worker",
        ModelTier::Standard,
        None,
        Vec::new(),
        1,
    )?;
    wake_entry.execution_mode = WakeExecutionMode::Workspace;
    wake_entry.workspace_tool_palette = vec!["proxima-workspace/shell".into()];
    pg.set_wake_entries(&SetWakeEntriesRequest {
        owner: owner.clone(),
        personality_instance_id: worker,
        entries: vec![wake_entry],
    })
    .await?;
    Ok(())
}
