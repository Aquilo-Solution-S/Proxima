//! M3.B.4 done-when — `Engine::query` with `SupersessionStatus::HeadsOnly`
//! against a stateful Fact schema returns the latest observation per
//! natural-key tuple.
//!
//! The test seeds:
//! - 3 `file-revision-v1` Facts under `NK = (repo_id, file_path_a)`
//! - 1 `file-revision-v1` Fact under `NK = (repo_id, file_path_b)`
//!
//! Heads-only must return 2 rows: the most recent of `NK_a` + `NK_b`.

#![allow(clippy::too_many_arguments)]

use std::sync::Arc;
use std::time::Duration;

use proxima_code::{CodeChunkV1, CommitV1, FileRevisionV1, FileState};
use proxima_core::auth::{Credentials, NoAuth};
use proxima_core::engine::Engine;
use proxima_core::storage::Storage;
use proxima_core::verbs::event_ingest::{CitationMappingHint, CitedObjectHint, EventDraft};
use proxima_core::verbs::query::{
    MemoryStore, PersonalityRootFilter, QueryRequest, SupersessionStatus, TombstoneFilter,
};
use proxima_core::verbs::schema::{FlavorRegistryFrozen, PayloadKind, SchemaInfo, SchemaTombstone};
use proxima_core::{
    CORE_DERIVED_FROM_RELATION, FactPayload, FlavorRegistry, OrgId, Owner, Principal, SchemaId,
    SchemaVersion, SourceBatchId, SourceId, UserId,
};
use proxima_storage_pg::PgStorage;
use sqlx::{Connection, Executor, PgConnection, PgPool};
use uuid::Uuid;

const ADMIN_URL: &str = "postgres://proxima:proxima@localhost/proxima";

async fn create_db(name: &str) -> Result<(), sqlx::Error> {
    let mut conn = PgConnection::connect(ADMIN_URL).await?;
    conn.execute(format!("CREATE DATABASE \"{name}\"").as_str())
        .await?;
    conn.close().await?;
    Ok(())
}

async fn drop_db(name: &str) -> Result<(), sqlx::Error> {
    let mut conn = PgConnection::connect(ADMIN_URL).await?;
    conn.execute(format!("DROP DATABASE IF EXISTS \"{name}\"").as_str())
        .await?;
    conn.close().await?;
    Ok(())
}

fn make_owner() -> (UserId, Owner) {
    let user = UserId::new(Uuid::now_v7());
    let owner = Owner {
        principal: Principal::User(user),
        org_id: OrgId::new(Uuid::now_v7()),
    };
    (user, owner)
}

fn registry_for_test() -> FlavorRegistryFrozen {
    // Register the proxima-code schemas plus stub CitedObject / CitationMapping
    // schemas that EventIngest needs.
    let mut flavor = FlavorRegistry::new();
    proxima_code::register(&mut flavor);
    let mut frozen = flavor.freeze().list();
    frozen.push(SchemaInfo {
        schema_id: SchemaId::new("test/cited_blob".into()),
        schema_version: SchemaVersion::new(1),
        kind: PayloadKind::CitedObject,
        filter_keys: vec![],
        sidecar_table: None,
        natural_key_columns: vec![],
        tombstone: None,
        cbor_encoder: None,
    });
    frozen.push(SchemaInfo {
        schema_id: SchemaId::new("test/citation_blob".into()),
        schema_version: SchemaVersion::new(1),
        kind: PayloadKind::CitationMapping,
        filter_keys: vec![],
        sidecar_table: None,
        natural_key_columns: vec![],
        tombstone: None,
        cbor_encoder: None,
    });
    FlavorRegistryFrozen::with_schemas(frozen)
}

fn fresh_draft(owner: Owner, schema: &str, payload: &[u8]) -> EventDraft {
    let now = time::OffsetDateTime::now_utc();
    EventDraft {
        source_id: SourceId::new("test/source"),
        source_batch_id: SourceBatchId::new(Uuid::now_v7()),
        owner,
        schema_id: SchemaId::new(schema.into()),
        schema_version: SchemaVersion::new(1),
        payload: payload.to_vec(),
        observed_at: now,
        occurred_at: now,
        cited_object: CitedObjectHint {
            schema_id: SchemaId::new("test/cited_blob".into()),
            schema_version: SchemaVersion::new(1),
            content_hash: blake3::hash(payload).into(),
        },
        citation_mapping: CitationMappingHint {
            schema_id: SchemaId::new("test/citation_blob".into()),
            schema_version: SchemaVersion::new(1),
        },
    }
}

async fn seed_file_revision(
    pool: &PgPool,
    engine: &Engine,
    owner: Owner,
    repo_id: Uuid,
    file_path: &str,
    seed: &[u8],
) -> Result<Uuid, Box<dyn std::error::Error>> {
    seed_file_revision_state(
        pool,
        engine,
        owner,
        repo_id,
        file_path,
        seed,
        FileState::Present,
    )
    .await
}

async fn seed_file_revision_state(
    pool: &PgPool,
    engine: &Engine,
    owner: Owner,
    repo_id: Uuid,
    file_path: &str,
    seed: &[u8],
    state: FileState,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    // 1. EventIngest creates the memories row + supporting plumbing.
    let draft = fresh_draft(owner, FileRevisionV1::SCHEMA_ID, seed);
    let outcome = engine.event_ingest(&Credentials::None, draft).await?;
    let memory_id = outcome.memory_id.into_inner();

    // 2. Sidecar insert under (repo_id, file_path).
    sqlx::query(
        "INSERT INTO proxima_code.file_revision_v1 \
            (memory_id, repo_id, file_path, language, content_sha256, \
             size_bytes, indexed_commit_sha, state) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(memory_id)
    .bind(repo_id)
    .bind(file_path)
    .bind(Some("rust"))
    .bind(blake3::hash(seed).as_bytes().to_vec())
    .bind(i64::try_from(seed.len()).unwrap_or(i64::MAX))
    .bind("0000000000000000000000000000000000000000")
    .bind(state)
    .execute(pool)
    .await?;

    Ok(memory_id)
}

async fn seed_code_chunk_state(
    pool: &PgPool,
    engine: &Engine,
    owner: Owner,
    repo_id: Uuid,
    file_path: &str,
    chunk_index: i32,
    seed: &[u8],
    state: FileState,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let draft = fresh_draft(owner, CodeChunkV1::SCHEMA_ID, seed);
    let outcome = engine.event_ingest(&Credentials::None, draft).await?;
    let memory_id = outcome.memory_id.into_inner();
    sqlx::query(
        "INSERT INTO proxima_code.code_chunk_v1 \
            (memory_id, repo_id, file_path, chunk_index, text, language, chunk_type, \
             byte_range_start, byte_range_end, line_range_start, line_range_end, state) \
         VALUES ($1, $2, $3, $4, $5, $6, 'function', 0, $7, 1, 1, $8)",
    )
    .bind(memory_id)
    .bind(repo_id)
    .bind(file_path)
    .bind(chunk_index)
    .bind(String::from_utf8_lossy(seed).to_string())
    .bind(Some("rust"))
    .bind(i32::try_from(seed.len()).unwrap_or(i32::MAX))
    .bind(state)
    .execute(pool)
    .await?;
    Ok(memory_id)
}

async fn insert_memory_edge(
    pool: &PgPool,
    owner: &Owner,
    source_memory_id: Uuid,
    target_memory_id: Uuid,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let edge_id = Uuid::now_v7();
    let owner_kind = proxima_core::OwnerPrincipalKind::of(&owner.principal);
    let owner_principal_id = match &owner.principal {
        Principal::User(u) => u.into_inner(),
        Principal::Group(g) => g.into_inner(),
    };
    sqlx::query(
        "INSERT INTO proxima_core.edges \
            (edge_id, relation, relation_class, source_kind, source_memory_id, \
             target_kind, target_memory_id, authorship_kind, owner_principal_kind, \
             owner_principal_id, owner_org_id) \
         VALUES ($1, $2, 'Provenance', 'Fact', $3, 'Fact', $4, 'Engine', $5, $6, $7)",
    )
    .bind(edge_id)
    .bind(CORE_DERIVED_FROM_RELATION)
    .bind(source_memory_id)
    .bind(target_memory_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner.org_id.into_inner())
    .execute(pool)
    .await?;
    Ok(edge_id)
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn heads_only_returns_latest_per_natural_key() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = format!("postgres://proxima:proxima@localhost/{db_name}");

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        proxima_code::migrator().run(pg.pool()).await?;

        let storage: Arc<dyn Storage> = Arc::new(pg.clone());
        let (user, owner) = make_owner();

        let engine = Engine::new(
            registry_for_test(),
            MemoryStore::new(),
            Box::new(NoAuth::new(Principal::User(user), owner.clone())),
        )
        .with_storage(storage);

        let repo_id = Uuid::now_v7();

        // 3 revisions of file_a — same NK, increasing created_at.
        let _r1 = seed_file_revision(
            pg.pool(),
            &engine,
            owner.clone(),
            repo_id,
            "src/a.rs",
            b"v1",
        )
        .await?;
        tokio::time::sleep(Duration::from_millis(20)).await;
        let _r2 = seed_file_revision(
            pg.pool(),
            &engine,
            owner.clone(),
            repo_id,
            "src/a.rs",
            b"v2",
        )
        .await?;
        tokio::time::sleep(Duration::from_millis(20)).await;
        let r3 = seed_file_revision(
            pg.pool(),
            &engine,
            owner.clone(),
            repo_id,
            "src/a.rs",
            b"v3",
        )
        .await?;

        // 1 revision of file_b — distinct NK.
        tokio::time::sleep(Duration::from_millis(20)).await;
        let r_b = seed_file_revision(
            pg.pool(),
            &engine,
            owner.clone(),
            repo_id,
            "src/b.rs",
            b"b1",
        )
        .await?;

        // Heads-only query — engine populates stateful_heads from the
        // registered NK columns on FileRevisionV1.
        let req = QueryRequest {
            owner: owner.clone(),
            entity_kind: None,
            schema_id: Some(SchemaId::new(FileRevisionV1::SCHEMA_ID.into())),
            supersession: SupersessionStatus::HeadsOnly,
            tombstones: proxima_core::verbs::query::TombstoneFilter::PresentOnly,
            personality_roots: PersonalityRootFilter::IncludeInactive,
            limit: 100,
            include_payloads: true,
            memory_ids: Vec::new(),
            goal_ids: Vec::new(),
            edge_ids: Vec::new(),
            stateful_heads: Vec::new(),
        };
        let resp = engine.query(&Credentials::None, &req).await?;

        // Two heads: latest of NK_a (=r3) + sole row of NK_b (=r_b).
        assert_eq!(
            resp.memories.len(),
            2,
            "expected 2 heads, got {}: {:?}",
            resp.memories.len(),
            resp.memories.iter().map(|m| m.id).collect::<Vec<_>>()
        );
        let ids: Vec<Uuid> = resp.memories.iter().map(|m| m.id.into_inner()).collect();
        assert!(
            ids.contains(&r3),
            "expected latest NK_a head ({r3}) in heads"
        );
        assert!(ids.contains(&r_b), "expected NK_b head ({r_b}) in heads");

        // IncludeSuperseded — all 4 rows visible.
        let req_all = QueryRequest {
            owner: owner.clone(),
            entity_kind: None,
            schema_id: Some(SchemaId::new(FileRevisionV1::SCHEMA_ID.into())),
            supersession: SupersessionStatus::IncludeSuperseded,
            tombstones: proxima_core::verbs::query::TombstoneFilter::PresentOnly,
            personality_roots: PersonalityRootFilter::IncludeInactive,
            limit: 100,
            include_payloads: true,
            memory_ids: Vec::new(),
            goal_ids: Vec::new(),
            edge_ids: Vec::new(),
            stateful_heads: Vec::new(),
        };
        let resp_all = engine.query(&Credentials::None, &req_all).await?;
        assert_eq!(
            resp_all.memories.len(),
            4,
            "expected 4 rows with IncludeSuperseded"
        );

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("heads_only_returns_latest_per_natural_key failed");
}

#[tokio::test]
async fn heads_only_no_op_for_stateless_fact_schema() {
    // commit-v1 has no NK columns — heads-only should fall through to the
    // A/P-style supersedes scan, which (since Facts have no supersedes)
    // returns every row.
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = format!("postgres://proxima:proxima@localhost/{db_name}");

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        proxima_code::migrator().run(pg.pool()).await?;

        let storage: Arc<dyn Storage> = Arc::new(pg.clone());
        let (user, owner) = make_owner();

        let engine = Engine::new(
            registry_for_test(),
            MemoryStore::new(),
            Box::new(NoAuth::new(Principal::User(user), owner.clone())),
        )
        .with_storage(storage);

        // Two distinct commit Facts.
        for payload in [b"c1" as &[u8], b"c2"] {
            let draft = fresh_draft(owner.clone(), CommitV1::SCHEMA_ID, payload);
            engine.event_ingest(&Credentials::None, draft).await?;
        }

        let req = QueryRequest {
            owner: owner.clone(),
            entity_kind: None,
            schema_id: Some(SchemaId::new(CommitV1::SCHEMA_ID.into())),
            supersession: SupersessionStatus::HeadsOnly,
            tombstones: proxima_core::verbs::query::TombstoneFilter::PresentOnly,
            personality_roots: PersonalityRootFilter::IncludeInactive,
            limit: 100,
            include_payloads: true,
            memory_ids: Vec::new(),
            goal_ids: Vec::new(),
            edge_ids: Vec::new(),
            stateful_heads: Vec::new(),
        };
        let resp = engine.query(&Credentials::None, &req).await?;
        assert_eq!(
            resp.memories.len(),
            2,
            "stateless Facts: every row is a head"
        );

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("heads_only_no_op_for_stateless_fact_schema failed");
}

#[tokio::test]
async fn owner_snapshot_heads_only_folds_all_stateful_fact_schemas() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = format!("postgres://proxima:proxima@localhost/{db_name}");

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        proxima_code::migrator().run(pg.pool()).await?;
        let storage: Arc<dyn Storage> = Arc::new(pg.clone());
        let (user, owner) = make_owner();
        let engine = Engine::new(
            registry_for_test(),
            MemoryStore::new(),
            Box::new(NoAuth::new(Principal::User(user), owner.clone())),
        )
        .with_storage(storage);
        let repo_id = Uuid::now_v7();

        let a_v1 = seed_file_revision_state(
            pg.pool(),
            &engine,
            owner.clone(),
            repo_id,
            "src/a.rs",
            b"a1",
            FileState::Present,
        )
        .await?;
        tokio::time::sleep(Duration::from_millis(20)).await;
        let a_v2 = seed_file_revision_state(
            pg.pool(),
            &engine,
            owner.clone(),
            repo_id,
            "src/a.rs",
            b"a2",
            FileState::Present,
        )
        .await?;
        let c_v1 = seed_code_chunk_state(
            pg.pool(),
            &engine,
            owner.clone(),
            repo_id,
            "src/a.rs",
            0,
            b"c1",
            FileState::Present,
        )
        .await?;
        tokio::time::sleep(Duration::from_millis(20)).await;
        let c_v2 = seed_code_chunk_state(
            pg.pool(),
            &engine,
            owner.clone(),
            repo_id,
            "src/a.rs",
            0,
            b"c2",
            FileState::Present,
        )
        .await?;

        let mut req = QueryRequest::for_owner(owner.clone());
        req.limit = 100;
        let resp = engine.query(&Credentials::None, &req).await?;
        let ids = resp
            .memories
            .iter()
            .map(|m| m.id.into_inner())
            .collect::<Vec<_>>();
        assert!(!ids.contains(&a_v1));
        assert!(ids.contains(&a_v2));
        assert!(!ids.contains(&c_v1));
        assert!(ids.contains(&c_v2));
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("owner_snapshot_heads_only_folds_all_stateful_fact_schemas failed");
}

#[tokio::test]
async fn present_only_excludes_tombstone_head_without_reviving_previous_present() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = format!("postgres://proxima:proxima@localhost/{db_name}");

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        proxima_code::migrator().run(pg.pool()).await?;
        let storage: Arc<dyn Storage> = Arc::new(pg.clone());
        let (user, owner) = make_owner();
        let engine = Engine::new(
            registry_for_test(),
            MemoryStore::new(),
            Box::new(NoAuth::new(Principal::User(user), owner.clone())),
        )
        .with_storage(storage);
        let repo_id = Uuid::now_v7();

        let present = seed_file_revision_state(
            pg.pool(),
            &engine,
            owner.clone(),
            repo_id,
            "src/deleted.rs",
            b"v1",
            FileState::Present,
        )
        .await?;
        tokio::time::sleep(Duration::from_millis(20)).await;
        let tombstone = seed_file_revision_state(
            pg.pool(),
            &engine,
            owner.clone(),
            repo_id,
            "src/deleted.rs",
            b"v2",
            FileState::Tombstone,
        )
        .await?;

        let mut req = QueryRequest::for_owner(owner.clone());
        req.schema_id = Some(SchemaId::new(FileRevisionV1::SCHEMA_ID.into()));
        req.limit = 100;
        let resp = engine.query(&Credentials::None, &req).await?;
        let ids = resp
            .memories
            .iter()
            .map(|m| m.id.into_inner())
            .collect::<Vec<_>>();
        assert!(
            !ids.contains(&present),
            "older present row must not be revived"
        );
        assert!(
            !ids.contains(&tombstone),
            "default query hides tombstone head"
        );

        req.tombstones = TombstoneFilter::IncludeTombstoned;
        let resp = engine.query(&Credentials::None, &req).await?;
        assert_eq!(
            resp.memories
                .iter()
                .map(|m| m.id.into_inner())
                .collect::<Vec<_>>(),
            vec![tombstone],
        );

        req.supersession = SupersessionStatus::IncludeSuperseded;
        let resp = engine.query(&Credentials::None, &req).await?;
        let ids = resp
            .memories
            .iter()
            .map(|m| m.id.into_inner())
            .collect::<Vec<_>>();
        assert!(ids.contains(&present));
        assert!(ids.contains(&tombstone));

        req.tombstones = TombstoneFilter::PresentOnly;
        let resp = engine.query(&Credentials::None, &req).await?;
        let ids = resp
            .memories
            .iter()
            .map(|m| m.id.into_inner())
            .collect::<Vec<_>>();
        assert!(
            ids.contains(&present),
            "older Present row visible under IncludeSuperseded"
        );
        assert!(
            !ids.contains(&tombstone),
            "tombstone stays hidden under PresentOnly"
        );
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("present_only_excludes_tombstone_head_without_reviving_previous_present failed");
}

#[tokio::test]
async fn present_only_snapshot_excludes_edges_to_tombstoned_heads() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = format!("postgres://proxima:proxima@localhost/{db_name}");

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        proxima_code::migrator().run(pg.pool()).await?;
        let storage: Arc<dyn Storage> = Arc::new(pg.clone());
        let (user, owner) = make_owner();
        let engine = Engine::new(
            registry_for_test(),
            MemoryStore::new(),
            Box::new(NoAuth::new(Principal::User(user), owner.clone())),
        )
        .with_storage(storage);
        let repo_id = Uuid::now_v7();
        let active = seed_file_revision_state(
            pg.pool(),
            &engine,
            owner.clone(),
            repo_id,
            "src/live.rs",
            b"live",
            FileState::Present,
        )
        .await?;
        let deleted = seed_file_revision_state(
            pg.pool(),
            &engine,
            owner.clone(),
            repo_id,
            "src/deleted.rs",
            b"gone",
            FileState::Tombstone,
        )
        .await?;
        let edge_id = insert_memory_edge(pg.pool(), &owner, active, deleted).await?;

        let mut req = QueryRequest::for_owner(owner.clone());
        req.limit = 100;
        let resp = engine.query(&Credentials::None, &req).await?;
        assert!(resp.memories.iter().any(|m| m.id.into_inner() == active));
        assert!(!resp.memories.iter().any(|m| m.id.into_inner() == deleted));
        assert!(!resp.edges.iter().any(|e| e.id == edge_id));

        req.tombstones = TombstoneFilter::IncludeTombstoned;
        let resp = engine.query(&Credentials::None, &req).await?;
        assert!(resp.memories.iter().any(|m| m.id.into_inner() == deleted));
        assert!(resp.edges.iter().any(|e| e.id == edge_id));
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("present_only_snapshot_excludes_edges_to_tombstoned_heads failed");
}

#[tokio::test]
async fn present_only_edge_id_hydration_excludes_edges_with_hidden_endpoint() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = format!("postgres://proxima:proxima@localhost/{db_name}");

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        proxima_code::migrator().run(pg.pool()).await?;
        let storage: Arc<dyn Storage> = Arc::new(pg.clone());
        let (user, owner) = make_owner();
        let engine = Engine::new(
            registry_for_test(),
            MemoryStore::new(),
            Box::new(NoAuth::new(Principal::User(user), owner.clone())),
        )
        .with_storage(storage);
        let repo_id = Uuid::now_v7();
        let active = seed_file_revision_state(
            pg.pool(),
            &engine,
            owner.clone(),
            repo_id,
            "src/live.rs",
            b"live",
            FileState::Present,
        )
        .await?;
        let deleted = seed_file_revision_state(
            pg.pool(),
            &engine,
            owner.clone(),
            repo_id,
            "src/deleted.rs",
            b"gone",
            FileState::Tombstone,
        )
        .await?;
        let edge_id = insert_memory_edge(pg.pool(), &owner, active, deleted).await?;

        let mut req = QueryRequest::for_owner(owner.clone());
        req.edge_ids = vec![edge_id];
        req.limit = 1;
        let resp = engine.query(&Credentials::None, &req).await?;
        assert!(resp.edges.is_empty());

        req.tombstones = TombstoneFilter::IncludeTombstoned;
        let resp = engine.query(&Credentials::None, &req).await?;
        assert_eq!(resp.edges.len(), 1);
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("present_only_edge_id_hydration_excludes_edges_with_hidden_endpoint failed");
}

// Smoke check the registry-side wiring: code-chunk-v1 also registers NK
// columns. Lets us catch a regression where someone removes the override.
#[test]
fn flavor_registers_natural_keys() {
    let mut r = FlavorRegistry::new();
    proxima_code::register(&mut r);
    let registry = r.freeze();
    let nk_for = |sid: &str| {
        registry
            .lookup(&SchemaId::new(sid.into()), SchemaVersion::new(1))
            .map(|s| s.natural_key_columns.clone())
    };

    assert_eq!(
        nk_for(CommitV1::SCHEMA_ID).as_deref(),
        Some(&[][..] as &[String]),
        "commit-v1 must remain stateless"
    );
    assert_eq!(
        nk_for(FileRevisionV1::SCHEMA_ID),
        Some(vec!["repo_id".to_string(), "file_path".to_string()])
    );
    assert_eq!(
        nk_for(CodeChunkV1::SCHEMA_ID),
        Some(vec![
            "repo_id".to_string(),
            "file_path".to_string(),
            "chunk_index".to_string(),
        ])
    );
}

#[test]
fn flavor_registers_tombstone_discriminators() {
    let mut r = FlavorRegistry::new();
    proxima_code::register(&mut r);
    let registry = r.freeze();
    let tombstone_for = |sid: &str| {
        registry
            .lookup(&SchemaId::new(sid.into()), SchemaVersion::new(1))
            .and_then(|s| s.tombstone.clone())
    };
    assert_eq!(
        tombstone_for(FileRevisionV1::SCHEMA_ID),
        Some(SchemaTombstone {
            column: "state".into(),
            value: "Tombstone".into(),
        })
    );
    assert_eq!(
        tombstone_for(CodeChunkV1::SCHEMA_ID),
        Some(SchemaTombstone {
            column: "state".into(),
            value: "Tombstone".into(),
        })
    );
    assert_eq!(tombstone_for(CommitV1::SCHEMA_ID), None);
}
