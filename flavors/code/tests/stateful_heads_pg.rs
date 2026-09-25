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

use common::{migrated_db, seed_memory_with_sidecars_in_tx};
use proxima_code::{FileRevisionV1, FileState};
use proxima_core::engine::Engine;
use proxima_core::verbs::fact_ingest::{
    Citation, CitationMappingHint, CitedObjectHint, FactReceiptDraft, FactWriteCommand,
};
use proxima_core::verbs::query::{QueryRequest, SupersessionStatus};
use proxima_core::verbs::schema::{FlavorRegistryFrozen, PayloadKind};
use proxima_core::{
    FactPayload, FlavorRegistry, Owner, OwnerRef, PayloadKeyBuilder, SchemaId, SchemaVersion,
    SourceId, UserId,
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
    let mut registry = FlavorRegistry::new();
    proxima_code::register(&mut registry).expect("code schema registration");
    registry.add_fact_schema_or_panic_for_tests::<StatelessFactV1>();
    registry
        .try_add_opaque_schema(
            SchemaId::new(common::TEST_CITED_BLOB_SCHEMA_ID.into()),
            SchemaVersion::new(1),
            PayloadKind::CitedObject,
        )
        .expect("test cited-object registration");
    registry
        .try_add_opaque_schema(
            SchemaId::new(common::TEST_CITATION_BLOB_SCHEMA_ID.into()),
            SchemaVersion::new(1),
            PayloadKind::CitationMapping,
        )
        .expect("test citation-mapping registration");
    registry.freeze_or_panic_for_tests()
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct StatelessFactV1 {
    value: String,
}

impl FactPayload for StatelessFactV1 {
    const SCHEMA_ID: &'static str = "test/stateless-fact-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn receipt_key(&self) -> Vec<u8> {
        let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
        key.field_str("value", &self.value);
        key.finish()
    }

    fn render(&self) -> String {
        self.value.clone()
    }
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
        additional_references: Vec::new(),
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
    // The stamp and the row it promises land in one transaction: a memory row
    // that names a sidecar table it has no row in is refused at COMMIT.
    let mut stamped = pool.begin().await?;
    let (handle, t) = seed_memory_with_sidecars_in_tx(
        &mut stamped,
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
    .execute(&mut *stamped)
    .await?;
    stamped.commit().await?;

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

        // Two distinct sidecarless Facts.
        for payload in [b"c1" as &[u8], b"c2"] {
            let draft = fresh_draft(owner, StatelessFactV1::SCHEMA_ID, payload);
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
            entity_kind: None,
            schema_id: Some(SchemaId::new(StatelessFactV1::SCHEMA_ID.into())),
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
        let mut req = QueryRequest::readable();
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
