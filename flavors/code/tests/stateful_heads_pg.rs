//! `Engine::query` with `SupersessionStatus::HeadsOnly`
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

mod common;

use common::{code_registry_with_test_citations, migrated_db, seed_memory_with_sidecars};
use proxima_code::{CodeChunkV1, CommitV1, FileRevisionV1, FileState};
use proxima_core::engine::Engine;
use proxima_core::verbs::fact_ingest::{
    Citation, CitationMappingHint, CitedObjectHint, FactReceiptDraft, FactWriteCommand,
};
use proxima_core::verbs::query::{QueryRequest, SupersessionStatus};
use proxima_core::verbs::schema::{FlavorRegistryFrozen, PayloadKind, SchemaTombstone};
use proxima_core::{
    AbstractionPayload, FactPayload, FlavorRegistry, Owner, OwnerRef, SchemaId, SchemaVersion,
    SourceBatchId, SourceId, UserId,
};
use proxima_pg_testkit::drop_db;
use sqlx::PgPool;
use uuid::Uuid;

fn make_owner() -> (UserId, Owner) {
    let user = UserId::new(Uuid::now_v7());
    let owner = OwnerRef::Personal(user);
    (user, owner)
}

fn registry_for_test() -> FlavorRegistryFrozen {
    code_registry_with_test_citations()
}

fn fresh_draft(_owner: Owner, schema: &str, payload: &[u8]) -> FactWriteCommand {
    let now = time::OffsetDateTime::now_utc();
    FactWriteCommand {
        schema_id: SchemaId::new(schema.into()),
        schema_version: SchemaVersion::new(1),
        handle: None,
        source_id: None,
        ingest_key: None,
        payload: payload.to_vec(),
        rendered_text: None,
        lexical_language: None,
        receipt: Some(FactReceiptDraft {
            source_id: SourceId::new("test/source"),
            source_batch_id: SourceBatchId::new(Uuid::now_v7()),
            observed_at: now,
            occurred_at: now,
        }),
        citation: Some(Citation {
            object: CitedObjectHint {
                schema_id: SchemaId::new("test/cited_blob".into()),
                schema_version: SchemaVersion::new(1),
                content_hash: blake3::hash(payload).into(),
            },
            mapping: CitationMappingHint {
                schema_id: SchemaId::new("test/citation_blob".into()),
                schema_version: SchemaVersion::new(1),
            },
        }),
        derived_from: Vec::new(),
        refs: Vec::new(),
        blob_id: None,
        kind: "fact".into(),
    }
}

struct Seeded {
    handle: Uuid,
    t: Uuid,
}

async fn seed_file_revision(
    pool: &PgPool,
    owner: Owner,
    repo_id: Uuid,
    file_path: &str,
    seed: &[u8],
    handle: Option<Uuid>,
) -> Result<Seeded, Box<dyn std::error::Error>> {
    seed_file_revision_state(
        pool,
        owner,
        repo_id,
        file_path,
        seed,
        FileState::Present,
        handle,
    )
    .await
}

/// The admission is seeded, not ingested: `Engine::fact_ingest` writes no
/// flavor sidecar and so stamps none, and a hand-written
/// `file_revision_v1` row under it is a row `sidecar_tables` cannot reach.
async fn seed_file_revision_state(
    pool: &PgPool,
    owner: Owner,
    repo_id: Uuid,
    file_path: &str,
    seed: &[u8],
    state: FileState,
    handle: Option<Uuid>,
) -> Result<Seeded, Box<dyn std::error::Error>> {
    let (handle, t) = seed_memory_with_sidecars(
        pool,
        &owner,
        FileRevisionV1::SCHEMA_ID,
        "fact",
        None,
        handle,
        &[],
        &["proxima_code.file_revision_v1"],
    )
    .await?;

    sqlx::query(
        "INSERT INTO proxima_code.file_revision_v1 \
            (t, repo_id, file_path, language, content_sha256, \
             size_bytes, indexed_commit_sha, state) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(t)
    .bind(repo_id)
    .bind(file_path)
    .bind(Some("rust"))
    .bind(blake3::hash(seed).as_bytes().to_vec())
    .bind(i64::try_from(seed.len()).unwrap_or(i64::MAX))
    .bind("0000000000000000000000000000000000000000")
    .bind(state)
    .execute(pool)
    .await?;

    Ok(Seeded { handle, t })
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn heads_only_returns_latest_per_natural_key() {
    let (db_name, pg) = migrated_db().await;

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let storage = Arc::new(pg.clone()).storage_ports();
        let (_user, owner) = make_owner();

        let engine = Engine::new(registry_for_test()).with_storage_ports(storage);

        let repo_id = Uuid::now_v7();

        // 3 revisions of file_a — same handle, later t.
        let r1 = seed_file_revision(pg.pool_for_tests(), owner, repo_id, "src/a.rs", b"v1", None)
            .await?;
        tokio::time::sleep(Duration::from_millis(20)).await;
        let _r2 = seed_file_revision(
            pg.pool_for_tests(),
            owner,
            repo_id,
            "src/a.rs",
            b"v2",
            Some(r1.handle),
        )
        .await?;
        tokio::time::sleep(Duration::from_millis(20)).await;
        let r3 = seed_file_revision(
            pg.pool_for_tests(),
            owner,
            repo_id,
            "src/a.rs",
            b"v3",
            Some(r1.handle),
        )
        .await?;

        // 1 revision of file_b — distinct handle.
        tokio::time::sleep(Duration::from_millis(20)).await;
        let r_b = seed_file_revision(pg.pool_for_tests(), owner, repo_id, "src/b.rs", b"b1", None)
            .await?;

        // Heads-only query. FileRevisionV1's registered NK columns put
        // every revision of one path on one handle at ingest, so the head
        // scan needs no per-schema filter.
        let req = QueryRequest {
            owner,
            read_owners: vec![owner],
            entity_kind: None,
            schema_id: Some(SchemaId::new(FileRevisionV1::SCHEMA_ID.into())),
            supersession: SupersessionStatus::HeadsOnly,
            goal_state: None,
            assignment: None,
            evidence_contains: None,
            limit: 100,
            page: proxima_core::verbs::query::QueryPage::default(),
            include_payloads: true,
            memory_ids: Vec::new(),
            goal_ids: Vec::new(),
        };
        let resp = engine
            .query(
                &proxima_core::AuthzContext::single_owner(
                    &owner,
                    proxima_core::AuthPath::HostBearer,
                ),
                &req,
            )
            .await?;

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
            ids.contains(&r3.t),
            "expected latest file_a head ({}) in heads",
            r3.t
        );
        assert!(
            ids.contains(&r_b.t),
            "expected file_b head ({}) in heads",
            r_b.t
        );

        // IncludeSuperseded — all 4 rows visible.
        let req_all = QueryRequest {
            owner,
            read_owners: vec![owner],
            entity_kind: None,
            schema_id: Some(SchemaId::new(FileRevisionV1::SCHEMA_ID.into())),
            supersession: SupersessionStatus::IncludeSuperseded,
            goal_state: None,
            assignment: None,
            evidence_contains: None,
            limit: 100,
            page: proxima_core::verbs::query::QueryPage::default(),
            include_payloads: true,
            memory_ids: Vec::new(),
            goal_ids: Vec::new(),
        };
        let resp_all = engine
            .query(
                &proxima_core::AuthzContext::single_owner(
                    &owner,
                    proxima_core::AuthPath::HostBearer,
                ),
                &req_all,
            )
            .await?;
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
    let (db_name, pg) = migrated_db().await;

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let storage = Arc::new(pg.clone()).storage_ports();
        let (_user, owner) = make_owner();

        let engine = Engine::new(registry_for_test()).with_storage_ports(storage);

        // Two distinct commit Facts.
        for payload in [b"c1" as &[u8], b"c2"] {
            let draft = fresh_draft(owner, CommitV1::SCHEMA_ID, payload);
            engine
                .fact_ingest(
                    &proxima_core::AuthzContext::single_owner(
                        &owner,
                        proxima_core::AuthPath::HostBearer,
                    ),
                    draft,
                )
                .await?;
        }

        let req = QueryRequest {
            owner,
            read_owners: vec![owner],
            entity_kind: None,
            schema_id: Some(SchemaId::new(CommitV1::SCHEMA_ID.into())),
            supersession: SupersessionStatus::HeadsOnly,
            goal_state: None,
            assignment: None,
            evidence_contains: None,
            limit: 100,
            page: proxima_core::verbs::query::QueryPage::default(),
            include_payloads: true,
            memory_ids: Vec::new(),
            goal_ids: Vec::new(),
        };
        let resp = engine
            .query(
                &proxima_core::AuthzContext::single_owner(
                    &owner,
                    proxima_core::AuthPath::HostBearer,
                ),
                &req,
            )
            .await?;
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
async fn heads_only_supersedes_older_same_principal_nk_revision() {
    let (db_name, pg) = migrated_db().await;

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let storage = Arc::new(pg.clone()).storage_ports();
        let user = UserId::new(Uuid::now_v7());
        let owner: Owner = OwnerRef::Personal(user);
        let engine = Engine::new(registry_for_test()).with_storage_ports(storage);
        let repo_id = Uuid::now_v7();

        let first_memory = seed_file_revision_state(
            pg.pool_for_tests(),
            owner,
            repo_id,
            "src/shared.rs",
            b"rev-1",
            FileState::Present,
            None,
        )
        .await?;
        tokio::time::sleep(Duration::from_millis(20)).await;
        let second_memory = seed_file_revision_state(
            pg.pool_for_tests(),
            owner,
            repo_id,
            "src/shared.rs",
            b"rev-2",
            FileState::Present,
            Some(first_memory.handle),
        )
        .await?;

        let req = QueryRequest {
            owner: OwnerRef::Personal(user),
            read_owners: vec![owner],
            entity_kind: None,
            schema_id: Some(SchemaId::new(FileRevisionV1::SCHEMA_ID.into())),
            supersession: SupersessionStatus::HeadsOnly,
            goal_state: None,
            assignment: None,
            evidence_contains: None,
            limit: 100,
            page: proxima_core::verbs::query::QueryPage::default(),
            include_payloads: true,
            memory_ids: Vec::new(),
            goal_ids: Vec::new(),
        };
        let resp = engine
            .query(
                &proxima_core::AuthzContext::single_owner(
                    &owner,
                    proxima_core::AuthPath::HostBearer,
                ),
                &req,
            )
            .await?;
        let ids = resp
            .memories
            .iter()
            .map(|m| m.id.into_inner())
            .collect::<Vec<_>>();

        // Same owner, same NK: only the newer revision is a head.
        assert!(
            !ids.contains(&first_memory.t),
            "older same-handle revision is not the head"
        );
        assert!(
            ids.contains(&second_memory.t),
            "newer same-handle head remains visible"
        );
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("heads_only_supersedes_older_same_principal_nk_revision failed");
}

#[tokio::test]
async fn owner_snapshot_heads_only_folds_stateful_fact_schemas() {
    let (db_name, pg) = migrated_db().await;

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let storage = Arc::new(pg.clone()).storage_ports();
        let (_user, owner) = make_owner();
        let engine = Engine::new(registry_for_test()).with_storage_ports(storage);
        let repo_id = Uuid::now_v7();

        let a_v1 = seed_file_revision_state(
            pg.pool_for_tests(),
            owner,
            repo_id,
            "src/a.rs",
            b"a1",
            FileState::Present,
            None,
        )
        .await?;
        tokio::time::sleep(Duration::from_millis(20)).await;
        let a_v2 = seed_file_revision_state(
            pg.pool_for_tests(),
            owner,
            repo_id,
            "src/a.rs",
            b"a2",
            FileState::Present,
            Some(a_v1.handle),
        )
        .await?;
        let mut req = QueryRequest::for_owner(owner);
        req.limit = 100;
        let resp = engine
            .query(
                &proxima_core::AuthzContext::single_owner(
                    &owner,
                    proxima_core::AuthPath::HostBearer,
                ),
                &req,
            )
            .await?;
        let ids = resp
            .memories
            .iter()
            .map(|m| m.id.into_inner())
            .collect::<Vec<_>>();
        assert!(!ids.contains(&a_v1.t));
        assert!(ids.contains(&a_v2.t));
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("owner_snapshot_heads_only_folds_stateful_fact_schemas failed");
}

#[tokio::test]
async fn later_t_is_head_even_when_sidecar_state_is_tombstone() {
    let (db_name, pg) = migrated_db().await;

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let storage = Arc::new(pg.clone()).storage_ports();
        let (_user, owner) = make_owner();
        let authz =
            proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::HostBearer);
        let engine = Engine::new(registry_for_test()).with_storage_ports(storage);
        let repo_id = Uuid::now_v7();

        let present = seed_file_revision_state(
            pg.pool_for_tests(),
            owner,
            repo_id,
            "src/deleted.rs",
            b"v1",
            FileState::Present,
            None,
        )
        .await?;
        tokio::time::sleep(Duration::from_millis(20)).await;
        let tombstone = seed_file_revision_state(
            pg.pool_for_tests(),
            owner,
            repo_id,
            "src/deleted.rs",
            b"v2",
            FileState::Tombstone,
            Some(present.handle),
        )
        .await?;

        let mut req = QueryRequest::for_owner(owner);
        req.schema_id = Some(SchemaId::new(FileRevisionV1::SCHEMA_ID.into()));
        req.limit = 100;
        let resp = engine.query(&authz, &req).await?;
        let ids = resp
            .memories
            .iter()
            .map(|m| m.id.into_inner())
            .collect::<Vec<_>>();
        assert!(
            !ids.contains(&present.t),
            "older present row is not the head"
        );
        assert!(
            ids.contains(&tombstone.t),
            "sidecar tombstone is still the hot head"
        );

        req.supersession = SupersessionStatus::IncludeSuperseded;
        let resp = engine.query(&authz, &req).await?;
        let ids = resp
            .memories
            .iter()
            .map(|m| m.id.into_inner())
            .collect::<Vec<_>>();
        assert!(ids.contains(&present.t));
        assert!(ids.contains(&tombstone.t));
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("later_t_is_head_even_when_sidecar_state_is_tombstone failed");
}

// Smoke check the registry-side wiring: raw observations retain NK
// metadata, while code chunks are derived Abstractions rather than
// stateful Facts.
#[test]
fn flavor_registers_natural_keys() {
    let mut r = FlavorRegistry::new();
    proxima_code::register(&mut r).unwrap();
    let registry = r.freeze_or_panic_for_tests();
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
    let chunk_schema = registry
        .lookup(
            &SchemaId::new(<CodeChunkV1 as AbstractionPayload>::SCHEMA_ID.into()),
            SchemaVersion::new(1),
        )
        .expect("code chunk schema registered");
    assert_eq!(chunk_schema.kind, PayloadKind::Abstraction);
    assert!(chunk_schema.natural_key_columns.is_empty());
}

#[test]
fn flavor_registers_tombstone_discriminators() {
    let mut r = FlavorRegistry::new();
    proxima_code::register(&mut r).unwrap();
    let registry = r.freeze_or_panic_for_tests();
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
        tombstone_for(<CodeChunkV1 as AbstractionPayload>::SCHEMA_ID),
        None
    );
    assert_eq!(tombstone_for(CommitV1::SCHEMA_ID), None);
}
