//! One rule for reads and writes (#387): a role acts on kind K of an owner
//! in a direction when its limit for that direction is at least K.
//!
//! Every preset plus `new(G,A)` and `new(A,A)`, both directions, all four
//! kinds, on every read surface and write verb, served by the runtime role
//! under owner RLS, and embedding backfill. Then direct SQL under a
//! Fact-only scope on every owner-keyed table. Upload and forget need S3 and
//! run when `PROXIMA_S3_*` is set.

#[path = "fixtures/split_core_db.rs"]
mod split_core_db;

use std::collections::BTreeSet;
use std::sync::Arc;

use proxima::flavor::{FlavorBundle, NamedMigrator};
use proxima::{
    AppInfo, AuthzContext, BuiltProxima, ChangeHistoryRequest, CoreMcpTools, FactWrite, FlavorApp,
    GetMemoriesReadRequest, GetMemoryReadRequest, MemoryLineageDirection, MemoryLineageRequest,
    Proxima, QueryRequest, S3RuntimeConfig, ToolScope,
};
use proxima_blob_s3::CitedBlobUploadPrepareTs;
use proxima_core::error::{ErrorCode, ProtocolError};
use proxima_core::llm::BoundEmbeddingClient;
use proxima_core::storage_ports::{
    CitedBlobHeld, CitedBlobPort, CitedBlobReadUrl, CitedBlobService, CitedBlobStaged,
    CitedBlobUploadAborted, CitedBlobUploadPrepared,
};
use proxima_core::test_fixtures::{ConstantEmbedding, TestEmbeddingRouter};
use proxima_core::{
    AccessCeiling, AccessKind, AgentNoteV1, AuthPath, ChangeEventKind, ColdObjectStore,
    EdgeTargetProjection, EntityKind, EntityRef, FlavorRegistry, GroupId, MemoryId, Owner,
    OwnerRef, OwnerRoles, Role, StorageError, UploadedBlobPayload, UserId,
};
use proxima_pg_testkit::{db_url, drop_db, split_role_urls, unique_db_name};
use serde_json::{Value, json};
use split_core_db::create_split_core_db;
use sqlx::{Acquire, AssertSqlSafe, PgPool};
use uuid::Uuid;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

struct KindRuleApp;

impl FlavorBundle for KindRuleApp {
    fn register(_: &mut FlavorRegistry) -> Result<(), proxima_core::FlavorRegistryError> {
        Ok(())
    }

    fn migrators() -> Vec<NamedMigrator> {
        Vec::new()
    }
}

impl FlavorApp for KindRuleApp {
    fn app_info() -> AppInfo {
        AppInfo {
            id: "kind-rule-test",
            title: "Kind Rule Test",
            version: "1",
        }
    }
}

const KINDS: [EntityKind; 4] = [
    EntityKind::Fact,
    EntityKind::Abstraction,
    EntityKind::Perspective,
    EntityKind::Goal,
];

fn access(kind: EntityKind) -> AccessKind {
    AccessKind::from(kind)
}

/// Every preset, plus the two custom roles the issue names.
fn roles() -> [(&'static str, Role); 6] {
    [
        ("viewer", Role::viewer()),
        ("ingest", Role::ingest()),
        ("editor", Role::editor()),
        ("admin", Role::admin()),
        (
            "new(G,A)",
            Role::new(AccessCeiling::Goal, AccessCeiling::Abstraction, false).expect("role"),
        ),
        (
            "new(A,A)",
            Role::new(
                AccessCeiling::Abstraction,
                AccessCeiling::Abstraction,
                false,
            )
            .expect("role"),
        ),
    ]
}

fn context(group: OwnerRef, role: Role) -> AuthzContext {
    proxima_core::test_fixtures::authenticated_context(
        AuthzContext::for_subject_with_role(
            UserId::new(Uuid::now_v7()),
            [(group, role)],
            AuthPath::HostBearer,
        )
        .with_tool_scope(ToolScope::All),
    )
}

async fn call(
    tools: &CoreMcpTools,
    authz: &AuthzContext,
    owner: Owner,
    name: &str,
    args: Value,
) -> Result<Value, proxima::CoreMcpError> {
    tools
        .call_core_tool(
            authz.clone(),
            owner,
            Some("test-model".to_string()),
            name,
            args,
        )
        .await
}

fn handle_id(value: &Value) -> TestResult<Uuid> {
    let handle = value["handle"].as_str().ok_or("handle")?;
    let (_, id) = handle.split_once(':').ok_or("prefixed handle")?;
    Ok(id.parse()?)
}

struct Seeded {
    fact: Value,
    abstraction: Value,
    perspective: Value,
    /// An Abstraction nothing pins, which the SQL check cools and closes.
    unpinned: Value,
    /// A hot Abstraction nothing pins, which the SQL check tries to cool.
    spare: Value,
}

impl Seeded {
    fn ids(&self) -> TestResult<[(EntityKind, Uuid); 3]> {
        Ok([
            (EntityKind::Fact, handle_id(&self.fact)?),
            (EntityKind::Abstraction, handle_id(&self.abstraction)?),
            (EntityKind::Perspective, handle_id(&self.perspective)?),
        ])
    }
}

async fn remember(
    tools: &CoreMcpTools,
    authz: &AuthzContext,
    owner: Owner,
    title: &str,
) -> Result<Value, proxima::CoreMcpError> {
    call(
        tools,
        authz,
        owner,
        "core_remember",
        json!({"title": title, "body": "kindrule body", "tags": []}),
    )
    .await
}

async fn derive(
    tools: &CoreMcpTools,
    authz: &AuthzContext,
    owner: Owner,
    (kind, title): (EntityKind, &str),
    source: &Value,
) -> Result<Value, proxima::CoreMcpError> {
    call(
        tools,
        authz,
        owner,
        "core_derive",
        json!({
            "kind": kind.as_str(),
            "title": format!("kindrule {title}"),
            "body": "kindrule derived body",
            "tags": [],
            "source_handles": [source["handle"]],
            "model_id": "test-model",
        }),
    )
    .await
}

async fn set_goal(
    tools: &CoreMcpTools,
    authz: &AuthzContext,
    owner: Owner,
    seeded: &Seeded,
) -> Result<Value, proxima::CoreMcpError> {
    call(
        tools,
        authz,
        owner,
        "core_goal",
        json!({
            "action": "set",
            "schema_id": "core/simple-text-v1",
            "title": "kindrule goal",
            "text": "kindrule goal",
            "body": {},
            "evidence": [seeded.abstraction["handle"]],
            "target_perspective": seeded.perspective["handle"],
            "wake": {
                "trigger_schema_id": "core/agent-note-v1",
                "tool_ids": ["core_search_memories"],
                "prompt": "kindrule wake",
            },
        }),
    )
    .await
}

/// A Fact, an Abstraction over it, a Perspective over that and a Goal
/// assigned to the Perspective, all written by an admin of `group`.
async fn seed(tools: &CoreMcpTools, group: OwnerRef) -> TestResult<Seeded> {
    let admin = context(group, Role::admin());
    let fact = remember(tools, &admin, group, "kindrule fact").await?;
    let abstraction = derive(
        tools,
        &admin,
        group,
        (EntityKind::Abstraction, "abstraction"),
        &fact,
    )
    .await?;
    let perspective = derive(
        tools,
        &admin,
        group,
        (EntityKind::Perspective, "perspective"),
        &abstraction,
    )
    .await?;
    let unpinned = derive(
        tools,
        &admin,
        group,
        (EntityKind::Abstraction, "unpinned"),
        &fact,
    )
    .await?;
    let spare = derive(
        tools,
        &admin,
        group,
        (EntityKind::Abstraction, "spare"),
        &fact,
    )
    .await?;
    let seeded = Seeded {
        fact,
        abstraction,
        perspective,
        unpinned,
        spare,
    };
    set_goal(tools, &admin, group, &seeded).await?;
    call(
        tools,
        &admin,
        group,
        "core_interpret",
        json!({"claim": "kindrule interpretation", "confidence": 50,
               "subjects": [seeded.abstraction["handle"]]}),
    )
    .await?;
    call(
        tools,
        &admin,
        group,
        "core_goal",
        json!({
            "action": "set",
            "schema_id": "core/task-v1",
            "title": "kindrule task",
            "text": "kindrule task",
            "body": {"due_at": null, "priority": "High"},
            "evidence": [seeded.abstraction["handle"]],
            "target_perspective": seeded.perspective["handle"],
        }),
    )
    .await?;
    Ok(seeded)
}

/// Vectors for every seeded memory, written before the route has a client so
/// no background job races them (as `core_mcp_pg/hybrid_search.rs`).
async fn embed(admin: &PgPool, seeded: &Seeded, owner: Uuid) -> TestResult {
    let mut vector = vec![0.0_f32; 1024];
    vector[0] = 1.0;
    for (_, t) in seeded.ids()? {
        sqlx::query(
            "INSERT INTO proxima_core.embeddings
                (entity_id, model_id, dim, embedding_version, vec, owner_id)
             VALUES ($1, 'test-embed', 1024, 1, $2::real[]::vector, $3)",
        )
        .bind(t)
        .bind(&vector)
        .bind(owner)
        .execute(admin)
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.embedding_heads
                (entity_id, model_id, dim, embedding_version, owner_id)
             VALUES ($1, 'test-embed', 1024, 1, $2)",
        )
        .bind(t)
        .bind(owner)
        .execute(admin)
        .await?;
    }
    Ok(())
}

fn hidden(error: &ProtocolError) -> bool {
    matches!(error.code, ErrorCode::NotFound)
}

/// Every read surface for one role: what it returns is exactly the readable
/// kinds, and a row of an unreadable kind is `NotFound`.
#[expect(clippy::too_many_lines, reason = "one role, every read surface")]
async fn assert_reads(
    built: &BuiltProxima,
    tools: &CoreMcpTools,
    group: OwnerRef,
    name: &str,
    role: Role,
    seeded: &Seeded,
) -> TestResult {
    let authz = context(group, role);
    let engine = built.host().engine();
    let readable = |kind: EntityKind| role.may_read(access(kind));
    let ids = seeded.ids()?;

    for mode in ["lexical", "semantic", "hybrid"] {
        let out = call(
            tools,
            &authz,
            group,
            "core_search_memories",
            json!({"query": "kindrule", "mode": mode, "limit": 50,
                   "include_neighbor_edges": true}),
        )
        .await?;
        assert_eq!(out["degraded_to_lexical"], false, "{name} {mode}");
        let hits = out["memories"].as_array().ok_or("memories")?;
        for hit in hits {
            let kind = hit["kind"].as_str().ok_or("kind")?;
            assert!(
                KINDS.iter().any(|k| k.as_str() == kind && readable(*k)),
                "{name} {mode} returned a {kind}"
            );
        }
        for (kind, t) in ids {
            let found = hits.iter().any(|hit| hit["memory_id"] == t.to_string());
            assert_eq!(found, readable(kind), "{name} {mode} finds the {kind:?}");
        }
        let edges = serde_json::to_string(&out["neighbor_edges"])?;
        for (kind, t) in ids {
            if !readable(kind) {
                assert!(
                    !edges.contains(&t.to_string()),
                    "{name} {mode} edge names {kind:?}"
                );
            }
        }
    }

    for (kind, t) in ids {
        let read = engine
            .get_memory(
                &authz,
                &GetMemoryReadRequest {
                    memory_id: MemoryId::new(t),
                    include_neighbor_edges: true,
                },
            )
            .await;
        match read {
            Ok(response) => {
                assert!(readable(kind), "{name} reads a {kind:?}");
                assert_eq!(response.memory.map(|m| m.kind), Some(kind));
                for edge in response.neighbor_edges {
                    assert!(readable(edge.source.kind), "{name} neighbour {edge:?}");
                    if let EdgeTargetProjection::Visible { target } = edge.target {
                        assert!(readable(target.kind), "{name} neighbour {edge:?}");
                    }
                }
            }
            Err(error) => {
                assert!(
                    !readable(kind) && hidden(&error),
                    "{name} {kind:?}: {error}"
                );
            }
        }
    }
    let fact = MemoryId::new(ids[0].1);
    let neighbours = engine
        .get_memory(
            &authz,
            &GetMemoryReadRequest {
                memory_id: fact,
                include_neighbor_edges: true,
            },
        )
        .await?
        .neighbor_edges;
    let from_abstraction = neighbours
        .iter()
        .any(|edge| edge.source.entity == EntityRef::Memory(MemoryId::new(ids[1].1)));
    assert_eq!(
        from_abstraction,
        readable(EntityKind::Abstraction),
        "{name} sees the Abstraction's origin pin"
    );

    let batch = engine
        .get_memories(
            &authz,
            &GetMemoriesReadRequest {
                memory_ids: ids.iter().map(|(_, t)| MemoryId::new(*t)).collect(),
            },
        )
        .await?;
    let batch: BTreeSet<_> = batch.memories.iter().map(|m| m.kind.as_str()).collect();
    let expected: BTreeSet<_> = ids
        .iter()
        .filter(|(kind, _)| readable(*kind))
        .map(|(kind, _)| kind.as_str())
        .collect();
    assert_eq!(batch, expected, "{name} batch read");

    let descendants = engine
        .walk_memory_lineage(
            &authz,
            &MemoryLineageRequest {
                owner: group,
                start_memory_id: fact,
                direction: MemoryLineageDirection::Descendants,
                depth: 4,
                limit: 100,
                after: None,
            },
        )
        .await?;
    for node in &descendants.nodes {
        assert!(readable(node.kind), "{name} lineage node {node:?}");
    }
    for (kind, t) in &ids[1..] {
        let reached = descendants
            .nodes
            .iter()
            .any(|node| node.memory_id == MemoryId::new(*t));
        assert_eq!(reached, readable(*kind), "{name} lineage reaches {kind:?}");
    }
    let ancestors = engine
        .walk_memory_lineage(
            &authz,
            &MemoryLineageRequest {
                owner: group,
                start_memory_id: MemoryId::new(ids[2].1),
                direction: MemoryLineageDirection::Ancestors,
                depth: 4,
                limit: 100,
                after: None,
            },
        )
        .await;
    // A visible start projects itself at distance 0: an empty walk is the
    // engine's absent-or-invisible, which the wire reports as NotFound.
    match ancestors {
        Ok(walk) if walk.nodes.is_empty() => {
            assert!(!readable(EntityKind::Perspective), "{name} walks nowhere");
        }
        Ok(walk) => {
            assert!(
                readable(EntityKind::Perspective),
                "{name} walks from a Perspective"
            );
            assert!(walk.nodes.iter().all(|node| readable(node.kind)));
        }
        Err(error) => assert!(
            !readable(EntityKind::Perspective) && hidden(&error),
            "{name} walk: {error}"
        ),
    }

    let history = engine
        .change_history(
            &authz,
            &ChangeHistoryRequest {
                owner: group,
                limit: 1000,
                before: None,
            },
        )
        .await?;
    let seen: BTreeSet<_> = history
        .events
        .iter()
        .map(|event| match &event.kind {
            ChangeEventKind::EntityAppend { entity_kind, .. }
            | ChangeEventKind::EntityDelete { entity_kind, .. }
            | ChangeEventKind::EntityTransfer { entity_kind, .. } => entity_kind.as_str(),
        })
        .collect();
    let expected: BTreeSet<_> = KINDS
        .iter()
        .filter(|kind| readable(**kind))
        .map(|kind| kind.as_str())
        .collect();
    assert_eq!(seen, expected, "{name} change history");

    let memories = engine.query(&authz, &QueryRequest::readable()).await?;
    let seen: BTreeSet<_> = memories.memories.iter().map(|m| m.kind.as_str()).collect();
    let expected: BTreeSet<_> = KINDS[..3]
        .iter()
        .filter(|kind| readable(**kind))
        .map(|kind| kind.as_str())
        .collect();
    assert_eq!(seen, expected, "{name} query");
    for edge in &memories.edges {
        assert!(readable(edge.source.kind), "{name} query edge {edge:?}");
    }
    let goals = engine
        .query(
            &authz,
            &QueryRequest {
                entity_kind: Some(EntityKind::Goal),
                ..QueryRequest::readable()
            },
        )
        .await?;
    assert_eq!(
        goals.goals.iter().any(|goal| goal.title == "kindrule goal"),
        readable(EntityKind::Goal),
        "{name} goal read"
    );
    Ok(())
}

/// Every write verb for one role succeeds exactly when its write limit
/// covers the kind it writes.
async fn assert_writes(
    tools: &CoreMcpTools,
    group: OwnerRef,
    name: &str,
    role: Role,
    seeded: &Seeded,
) -> TestResult {
    let authz = context(group, role);
    let writes = |kind: EntityKind| role.may_write(access(kind));

    let fact = remember(tools, &authz, group, &format!("{name} fact")).await;
    assert_eq!(
        fact.is_ok(),
        writes(EntityKind::Fact),
        "{name} Fact: {fact:?}"
    );
    let abstraction = derive(
        tools,
        &authz,
        group,
        (EntityKind::Abstraction, &format!("{name} abstraction")),
        &seeded.fact,
    )
    .await;
    assert_eq!(
        abstraction.is_ok(),
        writes(EntityKind::Abstraction),
        "{name} Abstraction: {abstraction:?}"
    );
    let perspective = derive(
        tools,
        &authz,
        group,
        (EntityKind::Perspective, &format!("{name} perspective")),
        &seeded.abstraction,
    )
    .await;
    assert_eq!(
        perspective.is_ok(),
        writes(EntityKind::Perspective),
        "{name} Perspective: {perspective:?}"
    );
    let goal = set_goal(tools, &authz, group, seeded).await;
    assert_eq!(
        goal.is_ok(),
        writes(EntityKind::Goal),
        "{name} Goal: {goal:?}"
    );
    Ok(())
}

/// A blob port that stages whatever it is asked to: completion then runs the
/// engine's own gate and write path against Postgres, without S3.
#[derive(Debug)]
struct StagedBlobs;

#[async_trait::async_trait]
impl CitedBlobPort for StagedBlobs {
    async fn prepare_upload(
        &self,
        _: &AuthzContext,
        _: OwnerRef,
        _: &str,
        _: &str,
        _: u64,
    ) -> Result<CitedBlobUploadPrepared, StorageError> {
        Err(StorageError::Internal("completion only".into()))
    }

    async fn stage_upload(
        &self,
        _: &AuthzContext,
        _: OwnerRef,
        upload_id: &str,
    ) -> Result<CitedBlobStaged, StorageError> {
        // A distinct address per upload: the upload id's bytes, twice.
        let id = Uuid::parse_str(upload_id).map_err(|e| StorageError::Internal(e.to_string()))?;
        let mut content_hash = [0_u8; 32];
        content_hash[..16].copy_from_slice(id.as_bytes());
        content_hash[16..].copy_from_slice(id.as_bytes());
        Ok(CitedBlobStaged {
            payload: UploadedBlobPayload {
                content_hash,
                bucket: "kindrule".into(),
                object_key: format!("objects/{upload_id}"),
                sha256: content_hash,
                byte_len: 8,
                mime: "text/plain".into(),
                filename: "kindrule.txt".into(),
                etag: None,
                uploaded_at: time::OffsetDateTime::now_utc(),
            },
            already_completed: None,
        })
    }

    async fn finish_upload(
        &self,
        _: &AuthzContext,
        _: OwnerRef,
        _: &str,
        _: Uuid,
    ) -> Result<(), StorageError> {
        Ok(())
    }

    async fn abort_upload(
        &self,
        _: &AuthzContext,
        _: OwnerRef,
        _: &str,
    ) -> Result<CitedBlobUploadAborted, StorageError> {
        Ok(CitedBlobUploadAborted { aborted: true })
    }

    async fn read_url(
        &self,
        _: &AuthzContext,
        _: OwnerRef,
        _: Uuid,
    ) -> Result<CitedBlobReadUrl, StorageError> {
        Err(StorageError::Internal("completion only".into()))
    }

    async fn find_held_blobs(
        &self,
        _: &AuthzContext,
        _: OwnerRef,
        _: &[[u8; 32]],
    ) -> Result<Vec<CitedBlobHeld>, StorageError> {
        Ok(Vec::new())
    }
}

/// An upload Fact is a Fact write: completion succeeds exactly for the roles
/// that write Facts, `Role::ingest()` included.
async fn assert_upload(built: &BuiltProxima, group: OwnerRef, name: &str, role: Role) {
    let service = CitedBlobService::new(Arc::new(StagedBlobs));
    // The upload Fact's owner is the one writable owner of the context, so
    // the caller narrows to the space first, as `core_upload` does.
    let authz = context(group, role)
        .narrowed_to_owner(group)
        .expect("the role names the group");
    let completed = built
        .host()
        .engine()
        .complete_upload_as_fact(&service, &authz, group, &Uuid::now_v7().to_string(), &[])
        .await;
    assert_eq!(
        completed.is_ok(),
        role.may_write(AccessKind::Fact),
        "{name} upload: {completed:?}"
    );
}

/// Upload through S3 and forget: the S3-backed write verbs.
async fn assert_s3_writes(
    built: &BuiltProxima,
    tools: &CoreMcpTools,
    group: OwnerRef,
    name: &str,
    role: Role,
) -> TestResult {
    let body = format!("kindrule upload {name}");
    let authz = context(group, role)
        .narrowed_to_owner(group)
        .expect("the role names the group");
    let store = built.host().blobs().ok_or("S3 configured")?;
    let upload: TestResult = async {
        let prepared = store
            .prepare_upload(
                &authz,
                CitedBlobUploadPrepareTs {
                    owner: group,
                    filename: "kindrule.txt".into(),
                    mime: "text/plain".into(),
                    byte_len: body.len() as u64,
                },
            )
            .await?;
        store
            .cold_store()
            .put(&format!("pending/{}", prepared.upload_id), body.as_bytes())
            .await?;
        let service = CitedBlobService::new(Arc::new(store.clone()));
        built
            .host()
            .engine()
            .complete_upload_as_fact(&service, &authz, group, &prepared.upload_id, &[])
            .await?;
        Ok(())
    }
    .await;
    assert_eq!(
        upload.is_ok(),
        role.may_write(AccessKind::Fact),
        "{name} upload: {upload:?}"
    );

    // Fresh rows with no dependants, so each forget stands alone.
    let admin = context(group, Role::admin());
    let fact = remember(tools, &admin, group, &format!("{name} forget fact")).await?;
    let base = remember(tools, &admin, group, &format!("{name} forget base")).await?;
    let abstraction = derive(
        tools,
        &admin,
        group,
        (
            EntityKind::Abstraction,
            &format!("{name} forget abstraction"),
        ),
        &base,
    )
    .await?;
    let grounding = derive(
        tools,
        &admin,
        group,
        (EntityKind::Abstraction, &format!("{name} forget grounding")),
        &base,
    )
    .await?;
    let perspective = derive(
        tools,
        &admin,
        group,
        (
            EntityKind::Perspective,
            &format!("{name} forget perspective"),
        ),
        &grounding,
    )
    .await?;
    for (kind, target) in [
        (EntityKind::Perspective, perspective),
        (EntityKind::Abstraction, abstraction),
        (EntityKind::Fact, fact),
    ] {
        let forgot = call(
            tools,
            &context(group, role),
            group,
            "core_forget",
            json!({"memory": target["handle"]}),
        )
        .await;
        assert_eq!(
            forgot.is_ok(),
            role.may_write(access(kind)),
            "{name} forget {kind:?}: {forgot:?}"
        );
    }
    Ok(())
}

/// Embedding backfill queues exactly the kinds up to the caller's write
/// limit: a readable row above it is skipped, not refused mid-batch by the
/// kind-scoped job policy. Each role backfills a space no memory has a
/// vector in yet; a job the worker drains first leaves a vector head there
/// (written before the job is deleted), so either one counts.
async fn assert_backfill(
    built: &BuiltProxima,
    admin: &PgPool,
    router: &TestEmbeddingRouter,
    group: OwnerRef,
) -> TestResult {
    for (index, (name, role)) in roles().into_iter().enumerate() {
        let model = format!("kindrule-backfill-{index}");
        router.set_default(BoundEmbeddingClient::bind(Arc::new(
            ConstantEmbedding::prefixed(model.clone(), &[1.0, 0.0, 0.0]),
        ))?);
        let queued = built
            .host()
            .engine()
            .backfill_missing_embeddings(&context(group, role), &group, 1_000)
            .await;
        let kinds: Vec<String> = sqlx::query_scalar(
            "SELECT m.kind::text
               FROM proxima_core.memory m
              WHERE m.owner_id = $1
                AND (EXISTS (SELECT 1 FROM proxima_core.embedding_jobs j
                              WHERE j.entity_id = m.t AND j.model_id = $2)
                  OR EXISTS (SELECT 1 FROM proxima_core.embedding_heads h
                              WHERE h.entity_id = m.t AND h.model_id = $2))",
        )
        .bind(group.stored_owner_id())
        .bind(&model)
        .fetch_all(admin)
        .await?;
        let touched: BTreeSet<&str> = kinds.iter().map(String::as_str).collect();
        let expected: BTreeSet<&str> = [
            (EntityKind::Fact, "fact"),
            (EntityKind::Abstraction, "abstraction"),
            (EntityKind::Perspective, "perspective"),
        ]
        .into_iter()
        .filter(|(kind, _)| role.may_write(access(*kind)))
        .map(|(_, stored)| stored)
        .collect();
        assert_eq!(touched, expected, "{name} backfill: {queued:?}");
        match queued {
            Ok(count) => {
                assert!(role.may_write(AccessKind::Fact), "{name}");
                assert_eq!(count, kinds.len(), "{name}");
            }
            Err(error) => {
                assert!(!role.may_write(AccessKind::Fact), "{name}: {error:?}");
                assert_eq!(error.code, ErrorCode::Forbidden, "{name}: {error:?}");
            }
        }
    }
    Ok(())
}

async fn boot(
    database: &str,
    owner: Owner,
    router: Arc<TestEmbeddingRouter>,
    s3: Option<S3RuntimeConfig>,
) -> TestResult<BuiltProxima> {
    let (runtime_url, platform_url) = split_role_urls(database).await?;
    let mut app = Proxima::<KindRuleApp>::app()
        .database_url(runtime_url)
        .platform_database_url(platform_url)
        .owner(owner)
        .tool_scope(ToolScope::All)
        .embedding_router(router);
    if let Some(s3) = s3 {
        app = app.s3(s3);
    }
    Ok(app.build().await?)
}

#[tokio::test]
async fn every_role_reads_and_writes_exactly_up_to_its_limit() -> TestResult {
    let database = unique_db_name("proxima_kind_rule");
    create_split_core_db(&database).await?;
    let result: TestResult = async {
        let group = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
        let router = Arc::new(TestEmbeddingRouter::default());
        let s3 = S3RuntimeConfig::present_in_env()
            .then(|| {
                S3RuntimeConfig::from_env().map(|config| S3RuntimeConfig {
                    force_path_style: true,
                    ..config
                })
            })
            .transpose()?;
        let built = boot(&database, group, router.clone(), s3.clone()).await?;
        let tools = built.host().core_mcp_tools();
        let seeded = seed(&tools, group).await?;
        let admin = PgPool::connect(&db_url(&database)).await?;
        embed(&admin, &seeded, group.stored_owner_id()).await?;
        router.set_default(BoundEmbeddingClient::bind(Arc::new(
            ConstantEmbedding::prefixed("test-embed", &[1.0, 0.0, 0.0]),
        ))?);

        for (name, role) in roles() {
            assert_reads(&built, &tools, group, name, role, &seeded).await?;
            assert_writes(&tools, group, name, role, &seeded).await?;
            assert_upload(&built, group, name, role).await;
            if s3.is_some() {
                assert_s3_writes(&built, &tools, group, name, role).await?;
            } else {
                eprintln!("{name}: upload and forget skipped, PROXIMA_S3_* unset");
            }
        }

        assert_backfill(&built, &admin, &router, group).await?;
        assert_fact_scope_sql(&database, &admin, group, &seeded).await?;
        admin.close().await;
        built.shutdown().await;
        Ok(())
    }
    .await;
    drop_db(&database).await?;
    result
}

/// A context `for_system` built drives a write verb; a System context built
/// any other way is refused before storage.
#[tokio::test]
async fn only_a_for_system_context_writes_as_system() -> TestResult {
    let database = unique_db_name("proxima_kind_rule_system");
    create_split_core_db(&database).await?;
    let result: TestResult = async {
        let group = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
        let built = boot(
            &database,
            group,
            Arc::new(TestEmbeddingRouter::default()),
            None,
        )
        .await?;
        let subject = UserId::new(Uuid::now_v7());
        let roles = OwnerRoles::for_subject(subject, [(group, Role::admin())])?;
        let note = |title: &str| AgentNoteV1 {
            note_id: Uuid::now_v7(),
            title: title.into(),
            body: "system write".into(),
            tags: Vec::new(),
            idempotency_key: None,
        };
        let engine = built.host().engine();

        let system = AuthzContext::for_system(built.system_authority(), roles.clone());
        let written = note("for_system");
        engine
            .ingest_fact(&system, FactWrite::new(group, "test/kind-rule", &written))
            .await?;

        let forged = AuthzContext::server_resolved(roles, AuthPath::System);
        let refused = note("forged");
        let error = engine
            .ingest_fact(&forged, FactWrite::new(group, "test/kind-rule", &refused))
            .await
            .expect_err("a System context not from for_system writes nothing");
        assert_eq!(error.code, ErrorCode::Forbidden, "{error}");
        built.shutdown().await;
        Ok(())
    }
    .await;
    drop_db(&database).await?;
    result
}

/// What decides a `proxima_core` row's kind. Every base table is listed; the
/// census refuses one that is not.
#[derive(Clone, Copy, Debug)]
enum Rows {
    /// Its own `kind` column.
    Kind,
    /// Every row belongs to a Goal.
    Goal,
    /// The memory at this column.
    Memory(&'static str),
    /// A Fact schema's sidecar: the memory at `t`, only ever a Fact.
    FactSidecar,
    /// A Goal, or the hot or cooled memory at `t`.
    Announce,
    /// The hot or cooled admission naming it.
    Content,
    /// Its `memory_head`.
    ClosedHandle,
    /// Fact-only, owner-level or ownerless: no A/P/G row exists.
    NoKind,
}

const TABLES: &[(&str, Rows)] = &[
    ("agent_derivation_v1", Rows::Memory("t")),
    ("agent_note_v1", Rows::FactSidecar),
    ("announce", Rows::Announce),
    ("blob", Rows::NoKind),
    ("blob_uploads", Rows::NoKind),
    ("closed_handle", Rows::ClosedHandle),
    ("cold_purge_pending", Rows::NoKind),
    ("content", Rows::Content),
    ("cooled", Rows::Kind),
    ("delegated_authority_grants", Rows::NoKind),
    ("embedding_heads", Rows::Memory("entity_id")),
    ("embedding_jobs", Rows::Memory("entity_id")),
    ("embeddings", Rows::Memory("entity_id")),
    ("erased_pin_target", Rows::NoKind),
    ("flavor_surface", Rows::NoKind),
    ("goal", Rows::Goal),
    ("goal_head", Rows::Goal),
    ("goal_replay_declaration", Rows::Goal),
    ("group_memberships", Rows::NoKind),
    ("ingest_keys", Rows::NoKind),
    ("installation", Rows::NoKind),
    ("interpretation_v1", Rows::Memory("t")),
    ("lexical_default", Rows::NoKind),
    ("lexical_languages", Rows::NoKind),
    ("mcp_call_logged_v1", Rows::FactSidecar),
    ("memory", Rows::Kind),
    ("memory_head", Rows::Kind),
    ("owner_rls_epoch", Rows::NoKind),
    ("owners", Rows::NoKind),
    ("projection", Rows::Memory("memory_id")),
    ("publication_origin", Rows::NoKind),
    ("publication_outbox", Rows::NoKind),
    ("sketch", Rows::Kind),
    ("source_cursors", Rows::NoKind),
    ("task_goal_v1", Rows::Goal),
    ("utterance_v1", Rows::FactSidecar),
    ("wake_config", Rows::Goal),
    ("write_act_v1", Rows::FactSidecar),
];

/// Rows of an Abstraction, Perspective or Goal, as the superuser sees them.
fn above_fact(table: &str, rows: Rows) -> Option<String> {
    let memory_kind = |join: &str| {
        format!(
            "EXISTS (SELECT 1 FROM proxima_core.memory AS m WHERE {join} AND m.kind <> 'fact')
             OR EXISTS (SELECT 1 FROM proxima_core.cooled AS m WHERE {join} AND m.kind <> 'fact')"
        )
    };
    let predicate = match rows {
        Rows::Kind => "x.kind::text <> 'fact'".to_owned(),
        Rows::Goal => "true".to_owned(),
        Rows::Memory(column) => memory_kind(&format!("m.t = x.{column}")),
        Rows::Announce => format!("x.entity::text = 'goal' OR {}", memory_kind("m.t = x.t")),
        Rows::Content => memory_kind("m.content_id = x.content_id"),
        Rows::ClosedHandle => "EXISTS (SELECT 1 FROM proxima_core.memory_head AS h
                                WHERE h.handle = x.handle AND h.kind <> 'fact')"
            .to_owned(),
        Rows::FactSidecar | Rows::NoKind => return None,
    };
    Some(format!(
        "SELECT to_jsonb(x)::text FROM proxima_core.{table} AS x WHERE {predicate}"
    ))
}

/// One statement over one row, the row bound as `$1` (its `to_jsonb` text).
async fn run_on_row(
    connection: &mut sqlx::PgConnection,
    statement: String,
    row: &str,
) -> Result<sqlx::postgres::PgQueryResult, sqlx::Error> {
    // SQL-POLICY: fixed-fragment — table and column names come from TABLES
    // and pg_attribute; the row is bound.
    sqlx::query(AssertSqlSafe(statement))
        .bind(row)
        .execute(connection)
        .await
}

fn sqlstate(error: &sqlx::Error) -> Option<String> {
    error
        .as_database_error()
        .and_then(|db| db.code().map(std::borrow::Cow::into_owned))
}

/// Direct SQL as the runtime role with a Fact-only scope (`Role::ingest()`):
/// no owner-keyed table returns an Abstraction, Perspective or Goal row, and
/// none lets one be written.
#[expect(clippy::too_many_lines, reason = "one scope, every table")]
async fn assert_fact_scope_sql(
    database: &str,
    admin: &PgPool,
    group: OwnerRef,
    seeded: &Seeded,
) -> TestResult {
    let census: Vec<String> = sqlx::query_scalar(
        "SELECT c.relname::text FROM pg_class AS c
           JOIN pg_namespace AS n ON n.oid = c.relnamespace
          WHERE n.nspname = 'proxima_core' AND c.relkind IN ('r', 'p')
          ORDER BY 1",
    )
    .fetch_all(admin)
    .await?;
    let listed: Vec<String> = TABLES
        .iter()
        .map(|(table, _)| (*table).to_owned())
        .collect();
    assert_eq!(census, listed, "every proxima_core table has a row kind");

    // Rows the seeding cannot reach through a verb without S3.
    let abstraction = handle_id(&seeded.abstraction)?;
    sqlx::query(
        "INSERT INTO proxima_core.closed_handle (handle)
         SELECT handle FROM proxima_core.memory WHERE t = $1",
    )
    .bind(handle_id(&seeded.unpinned)?)
    .execute(admin)
    .await?;
    // Cool the unpinned Abstraction the way forget does: the cooled row seals
    // the hot row's identity, which then goes.
    let mut cool = admin.begin().await?;
    sqlx::query(
        "INSERT INTO proxima_core.cooled
             (t, handle, owner_id, kind, object_key, blob_id, content_id, source_id,
              ingest_key, origins, refs, goal_refs)
         SELECT t, handle, owner_id, kind, 'kindrule/cooled', blob_id, content_id, source_id,
                ingest_key, origins, refs, goal_refs
           FROM proxima_core.memory WHERE t = $1",
    )
    .bind(handle_id(&seeded.unpinned)?)
    .execute(&mut *cool)
    .await?;
    sqlx::query("DELETE FROM proxima_core.agent_derivation_v1 WHERE t = $1")
        .bind(handle_id(&seeded.unpinned)?)
        .execute(&mut *cool)
        .await?;
    sqlx::query("DELETE FROM proxima_core.memory WHERE t = $1")
        .bind(handle_id(&seeded.unpinned)?)
        .execute(&mut *cool)
        .await?;
    cool.commit().await?;
    sqlx::query(
        "INSERT INTO proxima_core.embedding_jobs (entity_id, model_id, owner_id, status, dim)
         VALUES ($1, 'test-embed', $2, 'failed_permanent', 1024)",
    )
    .bind(abstraction)
    .bind(group.stored_owner_id())
    .execute(admin)
    .await?;

    let (runtime_url, _) = split_role_urls(database).await?;
    let runtime = PgPool::connect(&runtime_url).await?;
    let ingest = context(group, Role::ingest());
    let scope = ingest.owner_scope().ok_or("authenticated scope")?;
    let admin_context = context(group, Role::admin());
    let full_scope = admin_context.owner_scope().ok_or("authenticated scope")?;
    let mut uncovered = Vec::new();
    let mut unprobed = Vec::new();
    for (table, rows) in TABLES {
        let Some(query) = above_fact(table, *rows) else {
            continue;
        };
        // SQL-POLICY: fixed-fragment — names come from TABLES and pg_attribute.
        let above: Vec<String> = sqlx::query_scalar(AssertSqlSafe(query))
            .fetch_all(admin)
            .await?;
        if above.is_empty() {
            uncovered.push(*table);
            continue;
        }
        // Writable columns, in order; the first one is the UPDATE target.
        let columns: Vec<String> = sqlx::query_scalar(
            "SELECT quote_ident(attname) FROM pg_attribute
              WHERE attrelid = format('proxima_core.%I', $1::text)::regclass
                AND attnum > 0 AND NOT attisdropped AND attgenerated = ''
              ORDER BY attnum",
        )
        .bind(table)
        .fetch_all(admin)
        .await?;
        let column = columns.first().ok_or("a writable column")?;
        let columns = columns.join(", ");

        let mut tx = proxima_storage_pg::begin_owner_transaction(&runtime, scope).await?;
        // SQL-POLICY: fixed-fragment — table names come from TABLES.
        let visible: BTreeSet<String> = sqlx::query_scalar(AssertSqlSafe(format!(
            "SELECT to_jsonb(x)::text FROM proxima_core.{table} AS x"
        )))
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .collect();
        for row in &above {
            assert!(!visible.contains(row), "{table} shows a Fact scope {row}");
        }
        for row in &above {
            let mut probe = tx.begin().await?;
            // SQL-POLICY: fixed-fragment — the table name comes from TABLES.
            let deleted = run_on_row(
                &mut probe,
                format!("DELETE FROM proxima_core.{table} AS x WHERE to_jsonb(x)::text = $1"),
                row,
            )
            .await?;
            assert_eq!(
                deleted.rows_affected(),
                0,
                "{table}: a Fact scope deletes {row}"
            );
            probe.rollback().await?;
            let mut probe = tx.begin().await?;
            let updated = run_on_row(
                &mut probe,
                format!(
                    "UPDATE proxima_core.{table} AS x SET {column} = x.{column}
                      WHERE to_jsonb(x)::text = $1"
                ),
                row,
            )
            .await?;
            assert_eq!(
                updated.rows_affected(),
                0,
                "{table}: a Fact scope updates {row}"
            );
            probe.rollback().await?;
        }
        // Content is written before the admission that names it; its kind is
        // the admission's, which the memory policy refuses.
        let mut inserts = if matches!(rows, Rows::Content) {
            Vec::new()
        } else {
            above.clone()
        };
        if *table == "cooled" {
            // A cooled row seals a hot one, so no copy of a cooled row is
            // insertable; the probe cools the hot spare instead.
            inserts.push(
                sqlx::query_scalar(
                    "SELECT to_jsonb(jsonb_populate_record(NULL::proxima_core.cooled,
                            to_jsonb(m) || jsonb_build_object('object_key', 'kindrule/probe',
                                                              'cooled_at', now())))::text
                       FROM proxima_core.memory AS m WHERE m.t = $1",
                )
                .bind(handle_id(&seeded.spare)?)
                .fetch_one(admin)
                .await?,
            );
        }
        let mut probed = 0;
        for row in &inserts {
            // A scope that writes every kind is stopped at most by the
            // duplicate; the Fact scope is refused before that, by the write
            // policy or by a BEFORE trigger that cannot see the parent row
            // either.
            let insert = format!(
                "INSERT INTO proxima_core.{table} ({columns})
                 SELECT {columns} FROM jsonb_populate_record(NULL::proxima_core.{table}, $1::jsonb)"
            );
            let mut full =
                proxima_storage_pg::begin_owner_transaction(&runtime, full_scope).await?;
            let written = run_on_row(&mut full, insert.clone(), row).await;
            full.rollback().await?;
            match written {
                Ok(_) => {}
                Err(error) if sqlstate(&error).as_deref() == Some("23505") => {}
                // Not insertable as a copy at all (a closed handle, say).
                Err(_) => continue,
            }
            let mut probe = tx.begin().await?;
            let refused = run_on_row(&mut probe, insert, row)
                .await
                .expect_err("a Fact scope writes no A/P/G row");
            probe.rollback().await?;
            assert_ne!(
                sqlstate(&refused).as_deref(),
                Some("23505"),
                "{table}: {refused}"
            );
            assert!(sqlstate(&refused).is_some(), "{table}: {refused}");
            probed += 1;
        }
        if !matches!(rows, Rows::Content) && probed == 0 {
            unprobed.push(*table);
        }
        tx.rollback().await?;
    }
    runtime.close().await;
    assert!(
        uncovered.is_empty(),
        "the fixture writes no A/P/G row into {uncovered:?}"
    );
    assert!(
        unprobed.is_empty(),
        "no A/P/G row of {unprobed:?} was probed"
    );
    Ok(())
}

/// A Goal is embeddable, so the embedding tables hold Goal rows too: a
/// Perspective limit reads and deletes a Memory's rows there, not a Goal's.
#[tokio::test]
async fn an_embedding_row_of_a_goal_needs_the_goal_limit() -> TestResult {
    let database = unique_db_name("proxima_kind_rule_goal_vec");
    create_split_core_db(&database).await?;
    let result: TestResult = async {
        let group = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
        let built = boot(&database, group, Arc::default(), None).await?;
        let seeded = seed(&built.host().core_mcp_tools(), group).await?;
        let admin = PgPool::connect(&db_url(&database)).await?;
        let owner_id = group.stored_owner_id();
        let goal: Uuid =
            sqlx::query_scalar("SELECT t FROM proxima_core.goal WHERE owner_id = $1 LIMIT 1")
                .bind(owner_id)
                .fetch_one(&admin)
                .await?;
        let fact = handle_id(&seeded.fact)?;
        for entity in [fact, goal] {
            sqlx::query(
                "INSERT INTO proxima_core.embeddings
                    (entity_id, model_id, dim, embedding_version, vec, owner_id)
                 VALUES ($1, 'goal-probe', 1024, 1, $3::real[]::vector, $2)",
            )
            .bind(entity)
            .bind(owner_id)
            .bind(vec![0.5_f32; 1024])
            .execute(&admin)
            .await?;
            sqlx::query(
                "INSERT INTO proxima_core.embedding_heads
                    (entity_id, model_id, dim, embedding_version, owner_id)
                 VALUES ($1, 'goal-probe', 1024, 1, $2)",
            )
            .bind(entity)
            .bind(owner_id)
            .execute(&admin)
            .await?;
            sqlx::query(
                "INSERT INTO proxima_core.embedding_jobs (entity_id, model_id, owner_id, status, dim)
                 VALUES ($1, 'goal-probe', $2, 'failed_permanent', 1024)",
            )
            .bind(entity)
            .bind(owner_id)
            .execute(&admin)
            .await?;
        }

        let (runtime_url, _) = split_role_urls(&database).await?;
        let runtime = PgPool::connect(&runtime_url).await?;
        let perspective = Role::new(AccessCeiling::Perspective, AccessCeiling::Perspective, false)
            .expect("role");
        for (name, role, expected) in [
            ("new(P,P)", perspective, BTreeSet::from([fact])),
            ("viewer", Role::viewer(), BTreeSet::from([fact, goal])),
        ] {
            let authz = context(group, role);
            let scope = authz.owner_scope().ok_or("authenticated scope")?;
            let mut tx = proxima_storage_pg::begin_owner_transaction(&runtime, scope).await?;
            let visible: BTreeSet<(String, Uuid)> = sqlx::query_as(
                "SELECT 'embeddings', entity_id FROM proxima_core.embeddings
                  WHERE model_id = 'goal-probe'
                 UNION ALL
                 SELECT 'embedding_heads', entity_id FROM proxima_core.embedding_heads
                  WHERE model_id = 'goal-probe'
                 UNION ALL
                 SELECT 'embedding_jobs', entity_id FROM proxima_core.embedding_jobs
                  WHERE model_id = 'goal-probe'",
            )
            .fetch_all(&mut *tx)
            .await?
            .into_iter()
            .collect();
            let expected: BTreeSet<(String, Uuid)> =
                ["embeddings", "embedding_heads", "embedding_jobs"]
                    .into_iter()
                    .flat_map(|table| expected.iter().map(move |t| (table.to_owned(), *t)))
                    .collect();
            assert_eq!(visible, expected, "{name} reads");
            tx.rollback().await?;
        }
        let authz = context(group, perspective);
        let scope = authz.owner_scope().ok_or("authenticated scope")?;
        let mut tx = proxima_storage_pg::begin_owner_transaction(&runtime, scope).await?;
        for (entity, expected) in [(goal, 0), (fact, 1)] {
            let deleted: (i64, i64, i64) = sqlx::query_as(
                "WITH e AS (DELETE FROM proxima_core.embeddings WHERE entity_id = $1 RETURNING 1),
                      h AS (DELETE FROM proxima_core.embedding_heads WHERE entity_id = $1 RETURNING 1),
                      j AS (DELETE FROM proxima_core.embedding_jobs WHERE entity_id = $1 RETURNING 1)
                 SELECT (SELECT count(*) FROM e), (SELECT count(*) FROM h), (SELECT count(*) FROM j)",
            )
            .bind(entity)
            .fetch_one(&mut *tx)
            .await?;
            assert_eq!(deleted, (expected, expected, expected), "new(P,P) deletes {entity}");
        }
        tx.rollback().await?;
        runtime.close().await;
        admin.close().await;
        built.shutdown().await;
        Ok(())
    }
    .await;
    drop_db(&database).await?;
    result
}

/// A Perspective and an Abstraction with the same payload get one Content row
/// each, so a scope that reads up to Abstractions reuses no Content it cannot
/// see: its derivation succeeds.
#[tokio::test]
async fn a_derivation_meets_no_content_above_its_limit() -> TestResult {
    let database = unique_db_name("proxima_kind_rule_content");
    create_split_core_db(&database).await?;
    let result: TestResult = async {
        let group = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
        let built = boot(
            &database,
            group,
            Arc::new(TestEmbeddingRouter::default()),
            None,
        )
        .await?;
        let tools = built.host().core_mcp_tools();
        let admin = context(group, Role::admin());
        let fact = remember(&tools, &admin, group, "kindrule twin fact").await?;
        let base = derive(
            &tools,
            &admin,
            group,
            (EntityKind::Abstraction, "twin base"),
            &fact,
        )
        .await?;
        let perspective = derive(
            &tools,
            &admin,
            group,
            (EntityKind::Perspective, "twin"),
            &base,
        )
        .await?;

        let abstractions = context(
            group,
            Role::new(
                AccessCeiling::Abstraction,
                AccessCeiling::Abstraction,
                false,
            )?,
        );
        let abstraction = derive(
            &tools,
            &abstractions,
            group,
            (EntityKind::Abstraction, "twin"),
            &base,
        )
        .await?;

        let pool = PgPool::connect(&db_url(&database)).await?;
        let contents: Vec<Option<Uuid>> = sqlx::query_scalar(
            "SELECT content_id FROM proxima_core.memory WHERE t = ANY($1) ORDER BY kind",
        )
        .bind(vec![handle_id(&abstraction)?, handle_id(&perspective)?])
        .fetch_all(&pool)
        .await?;
        assert_eq!(contents.len(), 2);
        assert!(contents.iter().all(Option::is_some), "{contents:?}");
        assert_ne!(contents[0], contents[1], "one Content row per kind");
        pool.close().await;
        built.shutdown().await;
        Ok(())
    }
    .await;
    drop_db(&database).await?;
    result
}
