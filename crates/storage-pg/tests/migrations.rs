//! Fresh v0.0.8 CREATE set. Requires local PG.

use std::collections::BTreeSet;
use std::path::Path;

use proxima_pg_testkit::{create_db, db_url, drop_db};
use proxima_storage_pg::{PgStorage, ensure_core_schema_markers};
use uuid::Uuid;

async fn table_exists(pg: &PgStorage, table_name: &str) -> bool {
    sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (
             SELECT 1
               FROM information_schema.tables
              WHERE table_schema = 'proxima_core'
                AND table_name = $1
         )",
    )
    .bind(table_name)
    .fetch_one(pg.pool_for_tests())
    .await
    .expect("table inventory query should succeed")
}

async fn column_exists(pg: &PgStorage, table: &str, column: &str) -> bool {
    sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (
             SELECT 1
               FROM information_schema.columns
              WHERE table_schema = 'proxima_core'
                AND table_name = $1
                AND column_name = $2
         )",
    )
    .bind(table)
    .bind(column)
    .fetch_one(pg.pool_for_tests())
    .await
    .expect("column inventory query should succeed")
}

async fn index_exists(pg: &PgStorage, index_name: &str) -> bool {
    sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (
             SELECT 1
               FROM pg_indexes
              WHERE schemaname = 'proxima_core'
                AND indexname = $1
         )",
    )
    .bind(index_name)
    .fetch_one(pg.pool_for_tests())
    .await
    .expect("index inventory query should succeed")
}

/// SQL keywords a relation name can follow. An identifier reached any other
/// way (a column, a function call, prose) is not a relation reference.
const RELATION_KEYWORDS: [&str; 5] = ["FROM", "INTO", "UPDATE", "JOIN", "TABLE"];

/// Relations named by SQL that no code path can reach: the three
/// `PgCitedObjectSidecar` / `PgCitationMappingSidecar` impls in
/// `sidecars/core_sidecars.rs`, which `register_core_pg_sidecars` never
/// registers, so their inserter slot is `None` and the statements never run.
/// The citation-mapping tables they name were dropped with the timeseries cut;
/// deleting the impls is a separate slice. `dead_sql_exclusions_are_still_live`
/// fails the moment that slice lands, so this list cannot rot into a
/// blanket pardon.
const DEAD_SQL_RELATIONS: [&str; 3] = [
    "citation_uploaded_blob_page_span_v1",
    "cited_mcp_call_io_v1",
    "cited_uploaded_blob_v1",
];

/// Every `proxima_core.<relation>` named by a keyword-led SQL fragment under
/// `crates/storage-pg/src`.
fn core_relations_named_in_storage_sql() -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    collect_relations(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src").as_path(),
        &mut found,
    );
    assert!(
        found.len() > 10,
        "the scan found only {found:?}; it stopped reading the sources it exists to read"
    );
    found
}

fn collect_relations(dir: &Path, found: &mut BTreeSet<String>) {
    let entries =
        std::fs::read_dir(dir).unwrap_or_else(|err| panic!("read {}: {err}", dir.display()));
    for entry in entries {
        let path = entry.expect("directory entry").path();
        if path.is_dir() {
            collect_relations(&path, found);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            let source = std::fs::read_to_string(&path)
                .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
            collect_relations_in_source(&source, found);
        }
    }
}

fn collect_relations_in_source(source: &str, found: &mut BTreeSet<String>) {
    for keyword in RELATION_KEYWORDS {
        let mut rest = source;
        while let Some(at) = rest.find(keyword) {
            let tail = &rest[at + keyword.len()..];
            rest = tail;
            if !tail.starts_with(|ch: char| ch.is_whitespace()) {
                continue;
            }
            let Some(qualified) = tail.trim_start().strip_prefix("proxima_core.") else {
                continue;
            };
            let name: String = qualified
                .chars()
                .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
                .collect();
            // `FROM proxima_core.lexical_tsv(...)` is a set-returning call,
            // not a relation.
            if !name.is_empty() && !qualified[name.len()..].starts_with('(') {
                found.insert(name);
            }
        }
    }
}

/// A relation this crate's SQL names but the migration does not create fails
/// at run time with 42P01 and nowhere else. `owner_fact_retention` spent the
/// whole v0.0.8 cut in that state: the table was dropped from the squashed
/// migration, the Rust surface survived, and `get_graph` read it on every
/// call.
#[tokio::test]
async fn every_core_relation_named_in_storage_sql_exists() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;

        let mut missing = Vec::new();
        for relation in core_relations_named_in_storage_sql() {
            if DEAD_SQL_RELATIONS.contains(&relation.as_str()) {
                continue;
            }
            let exists: bool =
                sqlx::query_scalar("SELECT to_regclass('proxima_core.' || $1::text) IS NOT NULL")
                    .bind(&relation)
                    .fetch_one(pg.pool_for_tests())
                    .await?;
            if !exists {
                missing.push(relation);
            }
        }
        assert!(
            missing.is_empty(),
            "storage-pg SQL names proxima_core relations the migration does not create: {missing:?}"
        );
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("core relation inventory test failed");
}

/// The dead-SQL exclusions above must stay dead *and* stay present. If a
/// relation leaves the scan its exclusion is obsolete; if the migration starts
/// creating one, it was never dead.
#[test]
fn dead_sql_exclusions_are_still_live() {
    let named = core_relations_named_in_storage_sql();
    for relation in DEAD_SQL_RELATIONS {
        assert!(
            named.contains(relation),
            "{relation} is no longer named by storage-pg SQL; drop it from DEAD_SQL_RELATIONS"
        );
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn migrations_apply_to_fresh_db() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());

    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }

    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;

        for table in [
            "owners",
            "memory",
            "memory_head",
            "ingest_keys",
            "announce",
            "owner_fact_retention",
            "owner_legal_holds",
            "cold_purge_pending",
            "compliance_audit_log",
            "delegated_authority_grants",
            "source_cursors",
        ] {
            assert!(
                table_exists(&pg, table).await,
                "empty apply must create proxima_core.{table}"
            );
        }
        for dead in [
            "edges",
            "fact_entities",
            "fact_receipts",
            "memories",
            "goals",
            "change_event",
            "compliance_suppression_keys",
        ] {
            assert!(
                !table_exists(&pg, dead).await,
                "v0.0.8 must not create dead table {dead}"
            );
        }

        assert!(
            !column_exists(&pg, "memory", "owner_kind").await,
            "owner_kind must not live on memory"
        );
        assert!(
            !column_exists(&pg, "embeddings", "owner_kind").await,
            "embeddings carry owner_id only"
        );
        assert!(
            !column_exists(&pg, "embeddings", "entity_kind").await,
            "embeddings carry entity_id only"
        );
        assert!(
            column_exists(&pg, "cooled", "source_id").await,
            "cooled carries source_id so source-scope erase can select"
        );
        assert!(
            column_exists(&pg, "cold_purge_pending", "compliance_operation_id").await,
            "cold purge debts must carry optional compliance attribution"
        );
        assert!(
            column_exists(&pg, "compliance_audit_log", "cold_object_purge_pending").await,
            "audit rows must expose exact-key purge debt independently"
        );
        assert!(
            column_exists(&pg, "cooled", "ingest_key").await,
            "cooled carries ingest_key next to source_id"
        );
        assert!(
            column_exists(&pg, "cooled", "blob_id").await,
            "cooled carries blob_id so citation-bearing replays keep their object"
        );
        assert!(
            column_exists(&pg, "memory", "schema_id").await,
            "schema_id is on each memory row (same value as the handle)"
        );
        assert!(
            column_exists(&pg, "memory", "sidecar_tables").await,
            "W4: forget dumps the per-t stamp, not the registry"
        );
        assert!(
            column_exists(&pg, "agent_note_v1", "embed_text").await,
            "W6: drain reads stored sidecar embed_text"
        );
        assert!(
            !column_exists(&pg, "memory", "schema_version").await,
            "no schema_version"
        );
        assert!(
            column_exists(&pg, "memory", "owner_id").await,
            "memory.owner_id is required"
        );
        assert!(
            !column_exists(&pg, "group_memberships", "created_at").await,
            "group_memberships has no created_at; list_group_members must not ORDER BY it"
        );

        for index in [
            "memory_owner_handle_t_idx",
            "memory_owner_t_handle_idx",
            "memory_owner_schema_t_idx",
            "memory_blob_id_idx",
            "memory_origins_gin",
            "memory_refs_gin",
            "memory_head_owner_schema_idx",
            "memory_head_owner_kind_idx",
            "group_memberships_member_user_id_idx",
            "embedding_jobs_pending_claim_idx",
            "owners_kind_idx",
            "announce_owner_seq_idx",
            "idx_embeddings_vec_hnsw",
            "embeddings_owner_model_idx",
            "goal_owner_state_t_idx",
            "agent_note_v1_search_tsv_gin",
            "utterance_v1_search_tsv_gin",
            "agent_derivation_v1_search_tsv_gin",
            "interpretation_v1_search_tsv_gin",
        ] {
            assert!(
                index_exists(&pg, index).await,
                "missing UML §7 index {index}"
            );
        }

        let core_versions: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM public._sqlx_migrations
              WHERE success AND version <= 9999",
        )
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(core_versions, 1, "v0.0.8 is one core migration");

        let grounding_vol: String = sqlx::query_scalar(
            "SELECT provolatile::text
               FROM pg_proc p
               JOIN pg_namespace n ON n.oid = p.pronamespace
              WHERE n.nspname = 'proxima_core'
                AND p.proname = 'pins_have_grounding_support'",
        )
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(
            grounding_vol, "v",
            "pins_have_grounding_support must be VOLATILE so B2 sees post-lock rows"
        );

        // The owners table is seeded by nothing: every owner row is minted
        // by a write (`ensure_owner_row`), including a transfer destination.
        let seeded_owners: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.owners")
                .fetch_one(pg.pool_for_tests())
                .await?;
        assert_eq!(seeded_owners, 0, "a fresh database seeds no owner rows");
        let owner_kinds: Vec<String> = sqlx::query_scalar(
            "SELECT e.enumlabel::text
               FROM pg_enum e
               JOIN pg_type t ON t.oid = e.enumtypid
               JOIN pg_namespace n ON n.oid = t.typnamespace
              WHERE n.nspname = 'proxima_core' AND t.typname = 'owner_kind'
              ORDER BY e.enumsortorder",
        )
        .fetch_all(pg.pool_for_tests())
        .await?;
        assert_eq!(
            owner_kinds,
            vec!["personal".to_string(), "group".to_string()],
            "owner_kind carries exactly the two owner kinds"
        );

        let owner_null: (i64,) = sqlx::query_as(
            "SELECT count(*)::bigint FROM information_schema.columns
              WHERE table_schema = 'proxima_core'
                AND table_name IN ('memory', 'memory_head', 'ingest_keys', 'announce', 'owners')
                AND column_name = 'owner_id'
                AND is_nullable = 'YES'",
        )
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(owner_null.0, 0, "owner_id must be NOT NULL");

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("migrations integration test failed");
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn memory_is_append_only_and_head_t_only() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let pool = pg.pool_for_tests();
        let owner = Uuid::now_v7();
        let handle = Uuid::now_v7();
        sqlx::query("INSERT INTO proxima_core.owners (owner_id, kind) VALUES ($1, 'personal')")
            .bind(owner)
            .execute(pool)
            .await?;
        let t = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
             VALUES ($1, 'fact', 'core/test-v1', $2, $3)",
        )
        .bind(handle)
        .bind(owner)
        .bind(t)
        .execute(pool)
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.memory (handle, t, kind, owner_id, schema_id, origins, refs)
             VALUES ($1, $2, 'fact', $3, 'core/test-v1', '{}', '{}')",
        )
        .bind(handle)
        .bind(t)
        .bind(owner)
        .execute(pool)
        .await?;

        let err = sqlx::query("UPDATE proxima_core.memory SET kind = 'abstraction' WHERE t = $1")
            .bind(t)
            .execute(pool)
            .await
            .expect_err("memory is append-only");
        assert!(err.to_string().contains("append-only"), "got: {err}");

        let later = Uuid::now_v7();
        sqlx::query("UPDATE proxima_core.memory_head SET t = $2 WHERE handle = $1")
            .bind(handle)
            .bind(later)
            .execute(pool)
            .await
            .expect("memory_head.t may move");

        let err = sqlx::query(
            "UPDATE proxima_core.memory_head SET kind = 'abstraction' WHERE handle = $1",
        )
        .bind(handle)
        .execute(pool)
        .await
        .expect_err("memory_head kind is frozen");
        assert!(err.to_string().contains("frozen"), "got: {err}");

        let err = sqlx::query(
            "UPDATE proxima_core.memory_head SET schema_id = 'other' WHERE handle = $1",
        )
        .bind(handle)
        .execute(pool)
        .await
        .expect_err("memory_head schema_id is frozen");
        assert!(err.to_string().contains("frozen"), "got: {err}");

        let destination = Uuid::now_v7();
        sqlx::query("INSERT INTO proxima_core.owners (owner_id, kind) VALUES ($1, 'group')")
            .bind(destination)
            .execute(pool)
            .await?;
        let mut tx = pool.begin().await?;
        sqlx::query("UPDATE proxima_core.memory_head SET owner_id = $2 WHERE handle = $1")
            .bind(handle)
            .bind(destination)
            .execute(&mut *tx)
            .await
            .expect("memory_head.owner_id may move for an owner transfer");
        sqlx::query("UPDATE proxima_core.memory SET owner_id = $2 WHERE t = $1")
            .bind(t)
            .bind(destination)
            .execute(&mut *tx)
            .await
            .expect("memory.owner_id may move for an owner transfer");
        tx.commit().await?;
        let moved: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.memory WHERE t = $1")
                .bind(t)
                .fetch_one(pool)
                .await?;
        assert_eq!(moved, destination);

        let err = sqlx::query(
            "INSERT INTO proxima_core.memory (handle, kind, owner_id, schema_id)
             VALUES ($1, 'abstraction', $2, 'core/test-v1')",
        )
        .bind(handle)
        .bind(owner)
        .execute(pool)
        .await
        .expect_err("kind must match head");
        assert!(
            err.to_string().contains("kind/owner") || err.to_string().contains("23514"),
            "got: {err}"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("append-only / head freeze test failed");
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn schema_markers_accept_fresh_schema_and_reject_incomplete_claim_lane() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());

    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }

    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        ensure_core_schema_markers(pg.pool_for_tests()).await?;

        sqlx::query(
            "ALTER TABLE proxima_core.cold_purge_pending
             RENAME COLUMN compliance_operation_id TO compliance_operation_id_old",
        )
        .execute(pg.pool_for_tests())
        .await?;
        let err = ensure_core_schema_markers(pg.pool_for_tests())
            .await
            .expect_err("missing cold purge attribution must reject --stamp");
        assert!(
            err.to_string()
                .contains("cold_purge_pending.compliance_operation_id"),
            "marker error must name the missing cold-purge attribution: {err}"
        );
        sqlx::query(
            "ALTER TABLE proxima_core.cold_purge_pending
             RENAME COLUMN compliance_operation_id_old TO compliance_operation_id",
        )
        .execute(pg.pool_for_tests())
        .await?;

        sqlx::query(
            "ALTER TABLE proxima_core.cold_purge_pending
             DROP CONSTRAINT cold_purge_pending_pkey",
        )
        .execute(pg.pool_for_tests())
        .await?;
        let err = ensure_core_schema_markers(pg.pool_for_tests())
            .await
            .expect_err("missing cold purge primary key must reject --stamp");
        assert!(
            err.to_string().contains("primary key"),
            "marker error must name the missing cold-purge primary key: {err}"
        );
        sqlx::query(
            "ALTER TABLE proxima_core.cold_purge_pending
             ADD PRIMARY KEY (object_key)",
        )
        .execute(pg.pool_for_tests())
        .await?;

        sqlx::query(
            "ALTER TABLE proxima_core.compliance_audit_log
             RENAME COLUMN cold_object_purge_pending TO cold_object_purge_pending_old",
        )
        .execute(pg.pool_for_tests())
        .await?;
        let err = ensure_core_schema_markers(pg.pool_for_tests())
            .await
            .expect_err("missing audit cold purge flag must reject --stamp");
        assert!(
            err.to_string().contains("cold_object_purge_pending"),
            "marker error must name the missing audit purge flag: {err}"
        );
        sqlx::query(
            "ALTER TABLE proxima_core.compliance_audit_log
             RENAME COLUMN cold_object_purge_pending_old TO cold_object_purge_pending",
        )
        .execute(pg.pool_for_tests())
        .await?;

        sqlx::query("ALTER TABLE proxima_core.cooled RENAME COLUMN blob_id TO blob_id_old")
            .execute(pg.pool_for_tests())
            .await?;
        let err = ensure_core_schema_markers(pg.pool_for_tests())
            .await
            .expect_err("missing cooled.blob_id must reject --stamp");
        assert!(
            err.to_string().contains("cooled.blob_id"),
            "marker error must name the missing cooled witness: {err}"
        );
        sqlx::query("ALTER TABLE proxima_core.cooled RENAME COLUMN blob_id_old TO blob_id")
            .execute(pg.pool_for_tests())
            .await?;

        sqlx::query(
            "ALTER TYPE proxima_core.embedding_job_status
             RENAME VALUE 'failed_permanent' TO 'failed_terminal'",
        )
        .execute(pg.pool_for_tests())
        .await?;
        let err = ensure_core_schema_markers(pg.pool_for_tests())
            .await
            .expect_err("incorrect embedding-job enum labels must reject --stamp");
        assert!(
            err.to_string().contains("labels/order"),
            "marker error must name the incorrect enum vocabulary: {err}"
        );
        sqlx::query(
            "ALTER TYPE proxima_core.embedding_job_status
             RENAME VALUE 'failed_terminal' TO 'failed_permanent'",
        )
        .execute(pg.pool_for_tests())
        .await?;

        sqlx::query(
            "ALTER TYPE proxima_core.announce_op
             RENAME VALUE 'transfer' TO 'transfer_old'",
        )
        .execute(pg.pool_for_tests())
        .await?;
        let err = ensure_core_schema_markers(pg.pool_for_tests())
            .await
            .expect_err("incorrect announce-op enum labels must reject --stamp");
        assert!(
            err.to_string().contains("announce_op labels/order"),
            "marker error must name the incorrect announce vocabulary: {err}"
        );
        sqlx::query(
            "ALTER TYPE proxima_core.announce_op
             RENAME VALUE 'transfer_old' TO 'transfer'",
        )
        .execute(pg.pool_for_tests())
        .await?;

        sqlx::query(
            "ALTER TABLE proxima_core.embedding_jobs
             DROP CONSTRAINT embedding_job_processing_claim_chk",
        )
        .execute(pg.pool_for_tests())
        .await?;
        let err = ensure_core_schema_markers(pg.pool_for_tests())
            .await
            .expect_err("missing processing-claim check must reject --stamp");
        assert!(
            err.to_string().contains("processing claim check"),
            "marker error must name the missing check: {err}"
        );

        sqlx::query(
            "ALTER TABLE proxima_core.embedding_jobs
             ADD CONSTRAINT embedding_job_processing_claim_chk CHECK (
                 (status = 'processing') = (claimed_at IS NOT NULL AND claim_token IS NOT NULL)
             ) NOT VALID",
        )
        .execute(pg.pool_for_tests())
        .await?;
        let err = ensure_core_schema_markers(pg.pool_for_tests())
            .await
            .expect_err("unvalidated processing-claim check must reject --stamp");
        assert!(
            err.to_string().contains("processing claim check"),
            "marker error must reject a NOT VALID claim check: {err}"
        );
        sqlx::query(
            "ALTER TABLE proxima_core.embedding_jobs
             VALIDATE CONSTRAINT embedding_job_processing_claim_chk",
        )
        .execute(pg.pool_for_tests())
        .await?;
        ensure_core_schema_markers(pg.pool_for_tests()).await?;
        sqlx::query(
            "ALTER TABLE proxima_core.embedding_jobs
             RENAME COLUMN claim_token TO claim_token_old",
        )
        .execute(pg.pool_for_tests())
        .await?;
        let err = ensure_core_schema_markers(pg.pool_for_tests())
            .await
            .expect_err("missing claim_token must reject --stamp");
        assert!(
            err.to_string()
                .contains("claim_token must be nullable uuid"),
            "marker error must name the incorrect column: {err}"
        );
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("schema marker validation test failed");
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn schema_markers_reject_damaged_lexical_default() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());

    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }

    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let pool = pg.pool_for_tests();
        ensure_core_schema_markers(pool).await?;

        sqlx::query(
            "ALTER TABLE proxima_core.lexical_default
             DROP CONSTRAINT lexical_default_pkey",
        )
        .execute(pool)
        .await?;
        let err = ensure_core_schema_markers(pool)
            .await
            .expect_err("lexical default without its singleton primary key must reject --stamp");
        assert!(
            err.to_string().contains("sole primary-key column"),
            "marker error must name the damaged lexical-default primary key: {err}"
        );
        sqlx::query(
            "ALTER TABLE proxima_core.lexical_default
             ADD CONSTRAINT lexical_default_pkey PRIMARY KEY (singleton)",
        )
        .execute(pool)
        .await?;

        sqlx::query(
            "ALTER TABLE proxima_core.lexical_default
             DROP CONSTRAINT lexical_default_singleton_check",
        )
        .execute(pool)
        .await?;
        let err = ensure_core_schema_markers(pool)
            .await
            .expect_err("lexical default without CHECK (singleton) must reject --stamp");
        assert!(
            err.to_string().contains("CHECK (singleton)"),
            "marker error must name the damaged lexical-default singleton check: {err}"
        );
        sqlx::query(
            "ALTER TABLE proxima_core.lexical_default
             ADD CONSTRAINT lexical_default_singleton_check CHECK (singleton)",
        )
        .execute(pool)
        .await?;

        sqlx::query(
            "ALTER TABLE proxima_core.lexical_default
             DROP CONSTRAINT lexical_default_config_fkey",
        )
        .execute(pool)
        .await?;
        let err = ensure_core_schema_markers(pool)
            .await
            .expect_err("lexical default without its active-language FK must reject --stamp");
        assert!(
            err.to_string()
                .contains("must reference lexical_languages(config)"),
            "marker error must name the damaged lexical-default foreign key: {err}"
        );
        sqlx::query(
            "ALTER TABLE proxima_core.lexical_default
             ADD CONSTRAINT lexical_default_config_fkey
             FOREIGN KEY (config) REFERENCES proxima_core.lexical_languages(config)",
        )
        .execute(pool)
        .await?;

        sqlx::query(
            "ALTER TABLE proxima_core.utterance_v1
             DROP CONSTRAINT utterance_v1_lexical_language_fkey",
        )
        .execute(pool)
        .await?;
        let err = ensure_core_schema_markers(pool)
            .await
            .expect_err("a stamped table without its language FK must reject --stamp");
        assert!(
            err.to_string()
                .contains("every stamped lexical_language column"),
            "marker error must name the missing stamped-language foreign key: {err}"
        );
        sqlx::query(
            "ALTER TABLE proxima_core.utterance_v1
             ADD CONSTRAINT utterance_v1_lexical_language_fkey
             FOREIGN KEY (lexical_language)
             REFERENCES proxima_core.lexical_languages(config)",
        )
        .execute(pool)
        .await?;

        sqlx::query("DELETE FROM proxima_core.lexical_default")
            .execute(pool)
            .await?;
        let err = ensure_core_schema_markers(pool)
            .await
            .expect_err("lexical default without its live singleton row must reject --stamp");
        assert!(
            err.to_string().contains("exactly one singleton=true row"),
            "marker error must name the missing lexical-default row: {err}"
        );

        sqlx::query("SELECT proxima_core.set_lexical_config('simple')")
            .execute(pool)
            .await?;
        ensure_core_schema_markers(pool).await?;
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("lexical default schema marker checks failed");
}

#[tokio::test]
async fn pre_v008_database_fails_closed() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());

    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }

    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;

        sqlx::query("CREATE SCHEMA proxima_core")
            .execute(pg.pool_for_tests())
            .await?;
        sqlx::query("CREATE TABLE proxima_core.edges (edge_id uuid NOT NULL)")
            .execute(pg.pool_for_tests())
            .await?;
        sqlx::query(
            "CREATE TABLE public._sqlx_migrations (
                 version bigint PRIMARY KEY,
                 description text NOT NULL,
                 installed_on timestamptz NOT NULL DEFAULT now(),
                 success boolean NOT NULL,
                 checksum bytea NOT NULL,
                 execution_time bigint NOT NULL
             )",
        )
        .execute(pg.pool_for_tests())
        .await?;
        sqlx::query(
            "INSERT INTO public._sqlx_migrations
                 (version, description, success, checksum, execution_time)
             VALUES
                 (1, 'init', true, decode('00', 'hex'), 0),
                 (5, 'group ownership access', true, decode('00', 'hex'), 0)",
        )
        .execute(pg.pool_for_tests())
        .await?;

        let err = pg
            .run_migrations()
            .await
            .expect_err("pre-v0.0.8 DB must fail closed");
        let msg = err.to_string();
        assert!(
            msg.contains("reset") || msg.contains("stamp") || msg.contains("v0.0.4"),
            "error must explain reset, got: {msg}",
        );

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("pre-v0.0.8 fail-closed test failed");
}
