//! The fresh CREATE set of the core migration. Requires local PG.

use std::collections::BTreeSet;
use std::path::Path;

use proxima_pg_testkit::{create_db, db_url, drop_db};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

trait DatabaseFixture {
    fn pool_for_tests(&self) -> &sqlx::PgPool;
}

impl DatabaseFixture for PgStorage {
    fn pool_for_tests(&self) -> &sqlx::PgPool {
        self.pool_for_tests()
    }
}

async fn apply_current_migrations(
    pg: &impl DatabaseFixture,
) -> Result<(), proxima_core::StorageError> {
    let database: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(pg.pool_for_tests())
        .await
        .map_err(|error| proxima_core::StorageError::Unavailable(error.to_string()))?;
    let (_, platform_url) = proxima_pg_testkit::split_role_urls(&database)
        .await
        .map_err(|error| proxima_core::StorageError::Unavailable(error.to_string()))?;
    let migration = proxima_storage_pg::PgMigrationStorage::connect_with_config(
        &platform_url,
        proxima_storage_pg::PgPoolConfig::default(),
        proxima_storage_pg::PgTuning::default(),
    )
    .await?;
    migration.run_migrations().await
}

async fn table_exists(pg: &impl DatabaseFixture, table_name: &str) -> bool {
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

async fn column_exists(pg: &impl DatabaseFixture, table: &str, column: &str) -> bool {
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

async fn index_exists(pg: &impl DatabaseFixture, index_name: &str) -> bool {
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

/// Every `proxima_core.<relation>` named by a keyword-led SQL fragment under
/// `crates/storage-pg/src`.
fn core_relations_named_in_storage_sql() -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    collect_relations(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src").as_path(),
        &mut found,
    );
    // These relations were removed by migration 0031; their historical
    // source references are intentionally retained only in release history.
    for retired in ["announce", "mcp_call_logged_v1", "wake_config"] {
        found.remove(retired);
    }
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
            let mut named = BTreeSet::new();
            collect_relations_in_source(&source, &mut named);
            // A file that CREATEs a relation is that relation's source, so
            // the migration is not required to be. This is how the in-crate
            // PG suites' fixture tables stay out of the claim without
            // pardoning the files that hold them: every other relation they
            // name is still checked.
            for created in relations_created_in_source(&source) {
                named.remove(&created);
            }
            found.extend(named);
        }
    }
}

/// Relations this source itself creates (`CREATE TABLE proxima_core.<name>`,
/// with or without `IF NOT EXISTS`).
fn relations_created_in_source(source: &str) -> BTreeSet<String> {
    let mut created = BTreeSet::new();
    let mut rest = source;
    while let Some(at) = rest.find("CREATE TABLE ") {
        let tail = &rest[at + "CREATE TABLE ".len()..];
        rest = tail;
        let tail = tail
            .trim_start()
            .strip_prefix("IF NOT EXISTS ")
            .unwrap_or(tail);
        let Some(qualified) = tail.trim_start().strip_prefix("proxima_core.") else {
            continue;
        };
        let name: String = qualified
            .chars()
            .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
            .collect();
        if !name.is_empty() {
            created.insert(name);
        }
    }
    created
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

/// A relation this crate's SQL names but the migration does not create fails at
/// run time with 42P01 and nowhere else: the Rust surface compiles, and the
/// first caller to reach it is the one that finds out.
#[tokio::test]
async fn every_core_relation_named_in_storage_sql_exists() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        apply_current_migrations(&pg).await?;

        let mut missing = Vec::new();
        for relation in core_relations_named_in_storage_sql() {
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

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn migrations_apply_to_fresh_db() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());

    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }

    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        apply_current_migrations(&pg).await?;

        for table in [
            "owners",
            "memory",
            "memory_head",
            "ingest_keys",
            "cold_purge_pending",
            "erased_pin_target",
            "delegated_authority_grants",
            "source_cursors",
        ] {
            assert!(
                table_exists(&pg, table).await,
                "empty apply must create proxima_core.{table}"
            );
        }
        for dead in [
            "compliance_audit_log",
            "owner_fact_retention",
            "owner_legal_holds",
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
                "the migration must not create dead table {dead}"
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
            !column_exists(&pg, "cold_purge_pending", "compliance_operation_id").await,
            "the purge queue is the debt; it attributes itself to no journal"
        );
        assert!(
            !column_exists(&pg, "erased_pin_target", "owner_id").await,
            "erased pin witnesses are owner-free technical metadata"
        );
        let witness_kinds: Vec<String> = sqlx::query_scalar(
            "SELECT e.enumlabel::text
               FROM pg_enum e
               JOIN pg_type t ON t.oid = e.enumtypid
               JOIN pg_namespace n ON n.oid = t.typnamespace
              WHERE n.nspname = 'proxima_core' AND t.typname = 'pin_target_kind'
              ORDER BY e.enumsortorder",
        )
        .fetch_all(pg.pool_for_tests())
        .await?;
        assert_eq!(
            witness_kinds,
            vec!["fact", "abstraction", "perspective", "goal"],
            "witness kind is the closed target vocabulary"
        );

        // A COMMENT is not a comment. `COMMENT ON` writes to pg_description,
        // which ships into every deployment's catalog and comes back out of
        // \d+, information_schema and every schema-dump tool an operator
        // points at the database. A statute named there is the substrate
        // asserting a legal position on behalf of a host it has never met —
        // and a migration is the one place a wrong claim is hardest to
        // retract, because it is already applied.
        //
        // Comments that describe the MECHANISM are welcome and there are
        // many. This asks only that none of them argue from a regulation.
        let statute_comments: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint
               FROM pg_description d
               JOIN pg_class c ON c.oid = d.objoid
               JOIN pg_namespace n ON n.oid = c.relnamespace
              WHERE n.nspname LIKE 'proxima%'
                AND (d.description ~* '(Art\\.|Article)\\s+[0-9]+'
                  OR d.description ~* '\\m(GDPR|DSGVO)\\M')",
        )
        .fetch_one(pg.pool_for_tests())
        .await
        .expect("pg_description scan");
        assert_eq!(
            statute_comments, 0,
            "no shipped catalog comment may name a statute; found {statute_comments}"
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
            column_exists(&pg, "cooled", "origins").await,
            "cooled carries nullable origins for exact replay"
        );
        assert!(
            column_exists(&pg, "cooled", "refs").await,
            "cooled carries nullable refs for exact replay"
        );
        assert!(
            column_exists(&pg, "cooled", "cold_digest").await,
            "cooled carries the nullable cold-object integrity witness"
        );
        let digest_constraint: bool = sqlx::query_scalar(
            "SELECT EXISTS (
                 SELECT 1
                   FROM pg_constraint c
                   JOIN pg_class r ON r.oid = c.conrelid
                   JOIN pg_namespace n ON n.oid = r.relnamespace
                  WHERE n.nspname = 'proxima_core'
                    AND r.relname = 'cooled'
                    AND c.conname = 'cooled_cold_digest_len_chk'
                    AND c.contype = 'c'
                    AND c.convalidated
             )",
        )
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert!(
            digest_constraint,
            "cooled digest witness length is constrained"
        );
        assert!(
            column_exists(&pg, "memory", "schema_id").await,
            "schema_id is on each memory row (same value as the handle)"
        );
        assert!(
            column_exists(&pg, "memory", "sidecar_tables").await,
            "forget dumps the per-t stamp, not the registry"
        );
        assert!(
            column_exists(&pg, "agent_note_v1", "embed_text").await,
            "drain reads stored sidecar embed_text"
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
            "embeddings_hnsw_d384",
            "embeddings_hnsw_d768",
            "embeddings_hnsw_d1024",
            "embeddings_hnsw_d1536",
            "embeddings_hnsw_d2048",
            "embeddings_hnsw_d3072",
            "embeddings_owner_model_idx",
            "goal_owner_state_t_idx",
            "sketch_owner_t_idx",
            // ONE composite GIN for the whole flavor, not one per sidecar.
            "core_projection_owner_tsv_gin",
        ] {
            assert!(
                index_exists(&pg, index).await,
                "missing declared index {index}"
            );
        }

        for retired in [
            // One partial index per width lane replaced the fixed-width one.
            "idx_embeddings_vec_hnsw",
            "agent_note_v1_search_tsv_gin",
            "utterance_v1_search_tsv_gin",
            "agent_derivation_v1_search_tsv_gin",
            "interpretation_v1_search_tsv_gin",
            "sketch_search_tsv_gin",
        ] {
            assert!(
                !index_exists(&pg, retired).await,
                "the projection replaced the per-sidecar GIN {retired}"
            );
        }

        assert!(
            table_exists(&pg, "projection").await,
            "flavor #0 declares a projection table"
        );
        for (table, column) in [
            ("agent_note_v1", "search_tsv"),
            ("agent_note_v1", "lexical_language"),
            ("utterance_v1", "search_tsv"),
            ("utterance_v1", "lexical_language"),
            ("agent_derivation_v1", "search_tsv"),
            ("agent_derivation_v1", "lexical_language"),
            ("interpretation_v1", "search_tsv"),
            ("interpretation_v1", "lexical_language"),
            ("sketch", "search_tsv"),
            ("sketch", "lexical_language"),
        ] {
            assert!(
                !column_exists(&pg, table, column).await,
                "the projection took {table}.{column} over"
            );
        }
        for column in [
            "memory_id",
            "schema_id",
            "owner_id",
            "search_tsv",
            "tag",
            "lexical_language",
        ] {
            assert!(
                column_exists(&pg, "projection", column).await,
                "the generator emits projection.{column}"
            );
        }

        // A fresh database applies the embedded core set and nothing else.
        // Derived from the migrator rather than a hardcoded count: the baseline was
        // one file, and every release after it appends one, so a literal here
        // would have to be edited by each additive migration and would say
        // nothing about which versions actually landed.
        let applied: Vec<i64> = sqlx::query_scalar(
            "SELECT version FROM public._sqlx_migrations
              WHERE success AND version <= 9999 ORDER BY version",
        )
        .fetch_all(pg.pool_for_tests())
        .await?;
        let embedded: Vec<i64> = proxima_storage_pg::core_migrator()
            .iter()
            .map(|migration| migration.version)
            .collect();
        assert_eq!(
            applied, embedded,
            "a fresh database applies exactly the embedded core migration set"
        );

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

/// The final embedding lanes reject unsupported dimensions and mismatched
/// vector widths.
#[tokio::test]
async fn embedding_lanes_refuse_unsupported_dimensions_and_widths() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name)
        .await
        .expect("PG is required for migrations tests");
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(db_url(&db_name)).await?;
        apply_current_migrations(&pg).await?;
        let pool = pg.pool_for_tests();
        let owner_id = Uuid::now_v7();
        sqlx::query("INSERT INTO proxima_core.owners (owner_id, kind) VALUES ($1, 'personal')")
            .bind(owner_id)
            .execute(pool)
            .await?;
        for (dim, width, check) in [
            (512_i16, 512_i32, "embeddings_dim_lane_chk"),
            (1024, 768, "embeddings_vec_width_chk"),
        ] {
            let err = sqlx::query(
                "INSERT INTO proxima_core.embeddings (entity_id, model_id, dim, vec, owner_id)
                 VALUES ($1, 'legacy-model', $2, array_fill(0.5::real, ARRAY[$3])::vector, $4)",
            )
            .bind(Uuid::now_v7())
            .bind(dim)
            .bind(width)
            .bind(owner_id)
            .execute(pool)
            .await
            .expect_err("the width CHECKs refuse the row");
            assert_eq!(
                err.as_database_error()
                    .and_then(sqlx::error::DatabaseError::code),
                Some("23514".into()),
                "{dim}/{width}: {err}"
            );
            assert!(err.to_string().contains(check), "{dim}/{width}: {err}");
        }
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("embedding lane check test failed");
}

/// A staged upload content hash remains exactly 32 bytes.
#[tokio::test]
async fn blob_upload_content_hash_rejects_noncanonical_length() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name)
        .await
        .expect("PG is required for migrations tests");
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(db_url(&db_name)).await?;
        apply_current_migrations(&pg).await?;
        let pool = pg.pool_for_tests();
        let owner = Uuid::now_v7();
        sqlx::query("INSERT INTO proxima_core.owners (owner_id, kind) VALUES ($1, 'personal')")
            .bind(owner)
            .execute(pool)
            .await?;
        let upload = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO proxima_core.blob_uploads
                (upload_id, owner_id, bucket, object_key, filename, mime,
                 expected_byte_len, expires_at)
             VALUES ($1, $2, 'bucket', 'objects/test', 'test',
                     'application/octet-stream', 1, now() + interval '1 day')",
        )
        .bind(upload)
        .bind(owner)
        .execute(pool)
        .await?;
        let malformed = sqlx::query(
            "UPDATE proxima_core.blob_uploads SET content_hash = decode('00', 'hex')
              WHERE upload_id = $1",
        )
        .bind(upload)
        .execute(pool)
        .await
        .expect_err("content identity is exactly 32 bytes");
        assert_eq!(
            malformed
                .as_database_error()
                .and_then(sqlx::error::DatabaseError::code),
            Some("23514".into())
        );
        assert!(
            malformed
                .to_string()
                .contains("blob_uploads_content_hash_chk"),
            "{malformed}"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("blob upload content hash test failed");
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn reference_integrity_migration_enforces_goal_refs_and_cooled_arrays() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        apply_current_migrations(&pg).await?;
        let pool = pg.pool_for_tests();
        let owner = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO proxima_core.owners (owner_id, kind)
             VALUES ($1, 'personal')",
        )
        .bind(owner)
        .execute(pool)
        .await?;

        let goal_handle = Uuid::now_v7();
        let goal_t = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO proxima_core.goal_head (handle, schema_id, owner_id, t)
             VALUES ($1, 'test/goal-v1', $2, $3)",
        )
        .bind(goal_handle)
        .bind(owner)
        .bind(goal_t)
        .execute(pool)
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.goal
                 (handle, t, owner_id, title, state, request_id, \"by\", via)
             VALUES ($1, $2, $3, 'reference target', 'Active', 'reference-goal', '00000000-0000-0000-0000-000000000509'::uuid, NULL)",
        )
        .bind(goal_handle)
        .bind(goal_t)
        .bind(owner)
        .execute(pool)
        .await?;

        let fact_handle = Uuid::now_v7();
        let fact_t = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
             VALUES ($1, 'fact', 'test/fact-v1', $2, $3)",
        )
        .bind(fact_handle)
        .bind(owner)
        .bind(fact_t)
        .execute(pool)
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.memory
                 (handle, t, kind, owner_id, schema_id, goal_refs, occurred_at, \"by\", via)
             VALUES ($1, $2, 'fact', $3, 'test/fact-v1', ARRAY[$4]::uuid[], uuid_extract_timestamp($2), '00000000-0000-0000-0000-000000000509'::uuid, NULL)",
        )
        .bind(fact_handle)
        .bind(fact_t)
        .bind(owner)
        .bind(goal_t)
        .execute(pool)
        .await?;

        // Origins are Memory-only. Include a real Fact origin so the
        // grounding check cannot be the reason this insert is rejected.
        let source_handle = Uuid::now_v7();
        let source_t = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
             VALUES ($1, 'fact', 'test/source-v1', $2, $3)",
        )
        .bind(source_handle)
        .bind(owner)
        .bind(source_t)
        .execute(pool)
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.memory
                 (handle, t, kind, owner_id, schema_id, occurred_at, \"by\", via)
             VALUES ($1, $2, 'fact', $3, 'test/source-v1', uuid_extract_timestamp($2), '00000000-0000-0000-0000-000000000509'::uuid, NULL)",
        )
        .bind(source_handle)
        .bind(source_t)
        .bind(owner)
        .execute(pool)
        .await?;

        let content_id: Uuid = sqlx::query_scalar(
            "INSERT INTO proxima_core.content (owner_id, schema_id, content_hash)
             VALUES ($1, 'test/abstraction-v1', $2)
             RETURNING content_id",
        )
        .bind(owner)
        .bind(vec![7_u8; 32])
        .fetch_one(pool)
        .await?;
        let abstraction_handle = Uuid::now_v7();
        let abstraction_t = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
             VALUES ($1, 'abstraction', 'test/abstraction-v1', $2, $3)",
        )
        .bind(abstraction_handle)
        .bind(owner)
        .bind(abstraction_t)
        .execute(pool)
        .await?;
        let err = sqlx::query(
            "INSERT INTO proxima_core.memory
                 (handle, t, kind, owner_id, schema_id, origins, content_id, \"by\", via)
             VALUES ($1, $2, 'abstraction', $3, 'test/abstraction-v1',
                     ARRAY[$4, $5]::uuid[], $6, '00000000-0000-0000-0000-000000000509'::uuid, NULL)",
        )
        .bind(abstraction_handle)
        .bind(abstraction_t)
        .bind(owner)
        .bind(source_t)
        .bind(goal_t)
        .bind(content_id)
        .execute(pool)
        .await
        .expect_err("a Goal cannot be an origin target");
        assert!(
            err.to_string().contains("origin pin")
                || err.to_string().contains("abstraction origins"),
            "Goal origin rejection must come from the origin-only checks: {err}"
        );

        let cooled_t = Uuid::now_v7();
        let err = sqlx::query(
            "INSERT INTO proxima_core.cooled
                 (t, handle, owner_id, kind, object_key, origins, \"by\", via)
             VALUES ($1, $2, $3, 'fact', 'cold/null-origin', ARRAY[NULL]::uuid[], '00000000-0000-0000-0000-000000000509'::uuid, NULL)",
        )
        .bind(cooled_t)
        .bind(Uuid::now_v7())
        .bind(owner)
        .execute(pool)
        .await
        .expect_err("cooled origins must reject NULL elements");
        assert!(
            err.to_string().contains("cooled_origins_no_null_chk"),
            "cooled origins constraint must name the malformed array: {err}"
        );

        let err = sqlx::query(
            "INSERT INTO proxima_core.cooled
                 (t, handle, owner_id, kind, object_key, refs, \"by\", via)
             VALUES ($1, $2, $3, 'fact', 'cold/null-ref', ARRAY[NULL]::uuid[], '00000000-0000-0000-0000-000000000509'::uuid, NULL)",
        )
        .bind(Uuid::now_v7())
        .bind(Uuid::now_v7())
        .bind(owner)
        .execute(pool)
        .await
        .expect_err("cooled refs must reject NULL elements");
        assert!(
            err.to_string().contains("cooled_refs_no_null_chk"),
            "cooled refs constraint must name the malformed array: {err}"
        );
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("reference-integrity migration test failed");
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn erased_pin_target_direct_insert_is_rejected_and_delete_records_kind() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        apply_current_migrations(&pg).await?;
        let pool = pg.pool_for_tests();
        let owner = Uuid::now_v7();
        let handle = Uuid::now_v7();
        let t = Uuid::now_v7();
        sqlx::query("INSERT INTO proxima_core.owners (owner_id, kind) VALUES ($1, 'personal')")
            .bind(owner)
            .execute(pool)
            .await?;
        let null_cooled_t = Uuid::now_v7();
        let err = sqlx::query(
            "INSERT INTO proxima_core.cooled
                 (t, handle, owner_id, kind, object_key, \"by\", via)
             VALUES ($1, $2, $3, 'fact', 'cold/unsealed-null', '00000000-0000-0000-0000-000000000509'::uuid, NULL)",
        )
        .bind(null_cooled_t)
        .bind(Uuid::now_v7())
        .bind(owner)
        .execute(pool)
        .await
        .expect_err("new cooled rows require a hot identity seal even with NULL pins");
        assert!(
            err.to_string().contains("does not seal its hot Memory"),
            "unsealed NULL-array cooled insert must be rejected: {err}"
        );
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
            "INSERT INTO proxima_core.memory
                 (handle, t, kind, owner_id, schema_id, occurred_at, \"by\", via)
             VALUES ($1, $2, 'fact', $3, 'core/test-v1', uuid_extract_timestamp($2), '00000000-0000-0000-0000-000000000509'::uuid, NULL)",
        )
        .bind(handle)
        .bind(t)
        .bind(owner)
        .execute(pool)
        .await?;

        let err = sqlx::query(
            "INSERT INTO proxima_core.cooled
                 (t, handle, owner_id, kind, object_key, origins, refs, \"by\", via)
             SELECT t, handle, owner_id, kind, 'cold/partial-goal-refs', origins, refs, \"by\", via
               FROM proxima_core.memory WHERE t = $1",
        )
        .bind(t)
        .execute(pool)
        .await
        .expect_err("a partial cooled declaration cannot pass the exact pin seal");
        assert!(
            err.to_string().contains("does not seal its hot Memory"),
            "partial cooled goal_refs must be rejected by the identity seal: {err}"
        );

        let err = sqlx::query(
            "INSERT INTO proxima_core.erased_pin_target (t, kind)
             VALUES ($1, 'fact')",
        )
        .bind(t)
        .execute(pool)
        .await
        .expect_err("a direct witness insert must be rejected");
        assert!(
            err.to_string()
                .contains("written only by a target deletion trigger"),
            "direct witness insert must name its trusted writer: {err}"
        );

        sqlx::query("DELETE FROM proxima_core.memory WHERE t = $1")
            .bind(t)
            .execute(pool)
            .await?;
        let kind: String = sqlx::query_scalar(
            "SELECT kind::text FROM proxima_core.erased_pin_target WHERE t = $1",
        )
        .bind(t)
        .fetch_one(pool)
        .await?;
        assert_eq!(kind, "fact");

        let err =
            sqlx::query("UPDATE proxima_core.erased_pin_target SET kind = 'goal' WHERE t = $1")
                .bind(t)
                .execute(pool)
                .await
                .expect_err("witnesses are immutable");
        assert!(err.to_string().contains("append-only"), "got: {err}");

        let err = sqlx::query("DELETE FROM proxima_core.erased_pin_target WHERE t = $1")
            .bind(t)
            .execute(pool)
            .await
            .expect_err("witnesses are permanent");
        assert!(err.to_string().contains("append-only"), "got: {err}");
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("erased pin target writer test failed");
}

/// A witness is compatible only with the kind it records.  This deliberately
/// corrupts that metadata inside a rolled-back fixture transaction so the
/// database path is exercised: the attempted hard delete must fail closed and
/// leave the original witness intact.  The same witness must also block Goal
/// identity reuse.
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn conflicting_witness_rejects_delete_and_goal_t_reuse() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        apply_current_migrations(&pg).await?;
        let pool = pg.pool_for_tests();
        let owner = Uuid::now_v7();
        let t = Uuid::now_v7();
        let handle = Uuid::now_v7();
        sqlx::query("INSERT INTO proxima_core.owners (owner_id, kind) VALUES ($1, 'personal')")
            .bind(owner)
            .execute(pool)
            .await?;
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
            "INSERT INTO proxima_core.memory (handle, t, kind, owner_id, schema_id, occurred_at, \"by\", via)
             VALUES ($1, $2, 'fact', $3, 'core/test-v1', uuid_extract_timestamp($2), '00000000-0000-0000-0000-000000000509'::uuid, NULL)",
        )
        .bind(handle)
        .bind(t)
        .bind(owner)
        .execute(pool)
        .await?;
        sqlx::query("DELETE FROM proxima_core.memory WHERE t = $1")
            .bind(t)
            .execute(pool)
            .await?;
        let witness: String = sqlx::query_scalar(
            "SELECT kind::text FROM proxima_core.erased_pin_target WHERE t = $1",
        )
        .bind(t)
        .fetch_one(pool)
        .await?;
        assert_eq!(witness, "fact");

        // Goal identity reuse is a whole-transaction refusal: the temporary
        // head must not survive the failed Goal insert.
        let mut tx = pool.begin().await?;
        let goal_handle = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO proxima_core.goal_head (handle, schema_id, owner_id, t)
             VALUES ($1, 'core/task-v1', $2, $3)",
        )
        .bind(goal_handle)
        .bind(owner)
        .bind(t)
        .execute(&mut *tx)
        .await?;
        let err = sqlx::query(
            "INSERT INTO proxima_core.goal
                 (handle, t, owner_id, title, state, request_id, \"by\", via)
             VALUES ($1, $2, $3, 'reused', 'Active', 'reused-witness', '00000000-0000-0000-0000-000000000509'::uuid, NULL)",
        )
        .bind(goal_handle)
        .bind(t)
        .bind(owner)
        .execute(&mut *tx)
        .await
        .expect_err("a Goal cannot reuse an erased Memory witness t");
        assert!(
            err.to_string().contains("collides") || err.to_string().contains("erased target"),
            "Goal t reuse must name the witness collision: {err}"
        );
        tx.rollback().await?;
        let goal_rows: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.goal_head WHERE handle = $1",
        )
        .bind(goal_handle)
        .fetch_one(pool)
        .await?;
        assert_eq!(goal_rows, 0, "failed Goal reuse leaves no head behind");

        let replacement_handle = Uuid::now_v7();
        let mut tx = pool.begin().await?;
        sqlx::query("ALTER TABLE proxima_core.memory DISABLE TRIGGER memory_pin_checks")
            .execute(&mut *tx)
            .await?;
        // Fixture-only resurrection creates a live row solely to drive the
        // hard-delete trigger against a conflicting witness kind.
        sqlx::query(
            "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
             VALUES ($1, 'fact', 'core/test-v1', $2, $3)",
        )
        .bind(replacement_handle)
        .bind(owner)
        .bind(t)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.memory (handle, t, kind, owner_id, schema_id, occurred_at, \"by\", via)
             VALUES ($1, $2, 'fact', $3, 'core/test-v1', uuid_extract_timestamp($2), '00000000-0000-0000-0000-000000000509'::uuid, NULL)",
        )
        .bind(replacement_handle)
        .bind(t)
        .bind(owner)
        .execute(&mut *tx)
        .await?;
        sqlx::query("ALTER TABLE proxima_core.memory ENABLE TRIGGER memory_pin_checks")
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;

        // Fixture corruption changes only the witness kind; all production
        // append-only protection is restored before the corrupted state is
        // committed, so the following DELETE observes a pre-existing conflict.
        let mut tx = pool.begin().await?;
        sqlx::query(
            "ALTER TABLE proxima_core.erased_pin_target
             DISABLE TRIGGER erased_pin_target_append_only",
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query("UPDATE proxima_core.erased_pin_target SET kind = 'goal' WHERE t = $1")
            .bind(t)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "ALTER TABLE proxima_core.erased_pin_target
             ENABLE TRIGGER erased_pin_target_append_only",
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;

        let mut tx = pool.begin().await?;
        let err = sqlx::query("DELETE FROM proxima_core.memory WHERE t = $1")
            .bind(t)
            .execute(&mut *tx)
            .await
            .expect_err("a wrong-kind witness must abort the hard delete");
        assert!(
            err.to_string().contains("already records kind")
                || err.to_string().contains("not fact"),
            "wrong-kind deletion must fail closed: {err}"
        );
        tx.rollback().await?;
        let witness_after: String = sqlx::query_scalar(
            "SELECT kind::text FROM proxima_core.erased_pin_target WHERE t = $1",
        )
        .bind(t)
        .fetch_one(pool)
        .await?;
        assert_eq!(
            witness_after, "goal",
            "a failed delete leaves the conflicting witness intact"
        );
        let replacement_rows: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.memory WHERE t = $1")
                .bind(t)
                .fetch_one(pool)
                .await?;
        assert_eq!(
            replacement_rows, 1,
            "a failed delete leaves the pre-existing live row intact"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("conflicting witness rollback test failed");
}

async fn assert_cooled_writer_seal(pool: &sqlx::PgPool, t: Uuid) -> Result<(), sqlx::Error> {
    for (by, via) in [
        (Uuid::now_v7(), None),
        (Uuid::from_u128(0x509), Some(Uuid::now_v7())),
    ] {
        let error = sqlx::query(
            "INSERT INTO proxima_core.cooled
                (t, handle, owner_id, kind, object_key, origins, refs, goal_refs, \"by\", via)
             SELECT t, handle, owner_id, kind, 'cold/forged-writer', origins, refs, goal_refs, $2, $3
               FROM proxima_core.memory WHERE t = $1",
        ).bind(t).bind(by).bind(via).execute(pool).await
            .expect_err("cooled writer must seal the hot writer");
        assert!(
            error.to_string().contains("does not seal its hot Memory"),
            "{error}"
        );
    }
    sqlx::query(
        "INSERT INTO proxima_core.cooled
             (t, handle, owner_id, kind, object_key, origins, refs, goal_refs, \"by\", via)
         SELECT t, handle, owner_id, kind, 'cold/sealed', origins, refs, goal_refs, \"by\", via
           FROM proxima_core.memory WHERE t = $1",
    )
    .bind(t)
    .execute(pool)
    .await?;

    for query in [
        sqlx::query("UPDATE proxima_core.cooled SET \"by\" = $2 WHERE t = $1"),
        sqlx::query("UPDATE proxima_core.cooled SET via = $2 WHERE t = $1"),
    ] {
        let error = query
            .bind(t)
            .bind(Uuid::now_v7())
            .execute(pool)
            .await
            .expect_err("cooled writer is sealed");
        assert!(error.to_string().contains("frozen"), "{error}");
    }
    Ok(())
}

async fn cool_restoration_fixture(
    pool: &sqlx::PgPool,
    via: Option<Uuid>,
) -> Result<Uuid, sqlx::Error> {
    let owner = Uuid::now_v7();
    let handle = Uuid::now_v7();
    let t = Uuid::now_v7();
    sqlx::query("INSERT INTO proxima_core.owners (owner_id, kind) VALUES ($1, 'personal')")
        .bind(owner)
        .execute(pool)
        .await?;
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
        "INSERT INTO proxima_core.memory
             (handle, t, kind, owner_id, schema_id, source_id, ingest_key, occurred_at, \"by\", via)
         VALUES ($1, $2, 'fact', $3, 'core/test-v1', 'restoration-source', $2::text,
                 uuid_extract_timestamp($2), $4, $5)",
    )
    .bind(handle)
    .bind(t)
    .bind(owner)
    .bind(Uuid::now_v7())
    .bind(via)
    .execute(pool)
    .await?;

    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO proxima_core.cooled
             (t, handle, owner_id, kind, object_key, blob_id, content_id, source_id,
              ingest_key, origins, refs, goal_refs, \"by\", via)
         SELECT t, handle, owner_id, kind, 'cold/restoration', blob_id, content_id,
                source_id, ingest_key, origins, refs, goal_refs, \"by\", via
           FROM proxima_core.memory WHERE t = $1",
    )
    .bind(t)
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM proxima_core.memory WHERE t = $1")
        .bind(t)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    let reversible: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM proxima_core.cooled WHERE t = $1)
            AND NOT EXISTS (SELECT 1 FROM proxima_core.memory WHERE t = $1)
            AND NOT EXISTS (SELECT 1 FROM proxima_core.erased_pin_target WHERE t = $1)",
    )
    .bind(t)
    .fetch_one(pool)
    .await?;
    assert!(reversible, "cooling retains only the restorable sealed row");
    Ok(t)
}

async fn restore_cooled_with_writer(
    pool: &sqlx::PgPool,
    t: Uuid,
    writer: (Uuid, Option<Uuid>),
) -> Result<sqlx::postgres::PgQueryResult, sqlx::Error> {
    sqlx::query(
        "INSERT INTO proxima_core.memory
             (handle, t, kind, owner_id, schema_id, blob_id, content_id, source_id,
              ingest_key, origins, refs, goal_refs, occurred_at, \"by\", via)
         SELECT handle, t, kind, owner_id, 'core/test-v1', blob_id, content_id,
                source_id, ingest_key, origins, refs, goal_refs, uuid_extract_timestamp(t), $2, $3
           FROM proxima_core.cooled WHERE t = $1",
    )
    .bind(t)
    .bind(writer.0)
    .bind(writer.1)
    .execute(pool)
    .await
}

async fn assert_cooled_restoration_writer(pool: &sqlx::PgPool, t: Uuid) -> Result<(), sqlx::Error> {
    let (by, via, cooled): (Uuid, Option<Uuid>, serde_json::Value) =
        sqlx::query_as("SELECT \"by\", via, to_jsonb(c) FROM proxima_core.cooled c WHERE t = $1")
            .bind(t)
            .fetch_one(pool)
            .await?;
    let mut changed = vec![(Uuid::now_v7(), via), (by, Some(Uuid::now_v7()))];
    if via.is_some() {
        changed.push((by, None));
    }
    for writer in changed {
        let error = restore_cooled_with_writer(pool, t, writer)
            .await
            .expect_err("restoration must retain the original writer pair");
        assert_eq!(
            error
                .as_database_error()
                .and_then(sqlx::error::DatabaseError::code),
            Some("23514".into()),
            "writer {writer:?}: {error}"
        );
        let (hot_rows, after): (i64, serde_json::Value) = sqlx::query_as(
            "SELECT (SELECT count(*) FROM proxima_core.memory WHERE t = $1), to_jsonb(c)
               FROM proxima_core.cooled c WHERE t = $1",
        )
        .bind(t)
        .fetch_one(pool)
        .await?;
        assert_eq!(hot_rows, 0, "a refused restoration leaves no hot row");
        assert_eq!(
            after, cooled,
            "a refused restoration preserves the cooled row"
        );
    }

    let restored = restore_cooled_with_writer(pool, t, (by, via)).await?;
    assert_eq!(restored.rows_affected(), 1);
    let writer: (Uuid, Option<Uuid>) =
        sqlx::query_as("SELECT \"by\", via FROM proxima_core.memory WHERE t = $1")
            .bind(t)
            .fetch_one(pool)
            .await?;
    assert_eq!(
        writer,
        (by, via),
        "matching attribution restores the hot row"
    );
    let after: serde_json::Value =
        sqlx::query_scalar("SELECT to_jsonb(c) FROM proxima_core.cooled c WHERE t = $1")
            .bind(t)
            .fetch_one(pool)
            .await?;
    assert_eq!(
        after, cooled,
        "restoration preserves the original cooled seal"
    );
    Ok(())
}

#[tokio::test]
async fn cooled_restoration_preserves_writer_attribution() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        apply_current_migrations(&pg).await?;
        for via in [None, Some(Uuid::now_v7())] {
            let t = cool_restoration_fixture(pg.pool_for_tests(), via).await?;
            assert_cooled_restoration_writer(pg.pool_for_tests(), t).await?;
        }
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("cooled restoration writer attribution test failed");
}

#[tokio::test]
async fn cooled_identity_seal_freezes_all_but_transfer_remaps() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        apply_current_migrations(&pg).await?;
        let pool = pg.pool_for_tests();
        let owner = Uuid::now_v7();
        let destination = Uuid::now_v7();
        let handle = Uuid::now_v7();
        let t = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO proxima_core.owners (owner_id, kind)
             VALUES ($1, 'personal'), ($2, 'group')",
        )
        .bind(owner)
        .bind(destination)
        .execute(pool)
        .await?;
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
            "INSERT INTO proxima_core.memory
                 (handle, t, kind, owner_id, schema_id, occurred_at, \"by\", via)
             VALUES ($1, $2, 'fact', $3, 'core/test-v1', uuid_extract_timestamp($2), '00000000-0000-0000-0000-000000000509'::uuid, NULL)",
        )
        .bind(handle)
        .bind(t)
        .bind(owner)
        .execute(pool)
        .await?;
        assert_cooled_writer_seal(pool, t).await?;
        sqlx::query("UPDATE proxima_core.cooled SET owner_id = $2 WHERE t = $1")
            .bind(t)
            .bind(destination)
            .execute(pool)
            .await?;
        let writer: (Uuid, Option<Uuid>) = sqlx::query_as(
            "SELECT \"by\", via FROM proxima_core.cooled WHERE t = $1",
        ).bind(t).fetch_one(pool).await?;
        assert_eq!(writer, (Uuid::from_u128(0x509), None));
        let err = sqlx::query("UPDATE proxima_core.cooled SET handle = $2 WHERE t = $1")
            .bind(t)
            .bind(Uuid::now_v7())
            .execute(pool)
            .await
            .expect_err("cooled handle is sealed");
        assert!(err.to_string().contains("frozen"), "got: {err}");
        let err =
            sqlx::query("UPDATE proxima_core.cooled SET origins = ARRAY[$2]::uuid[] WHERE t = $1")
                .bind(t)
                .bind(Uuid::now_v7())
                .execute(pool)
                .await
                .expect_err("cooled pins are sealed");
        assert!(err.to_string().contains("frozen"), "got: {err}");
        let err = sqlx::query(
            "UPDATE proxima_core.cooled SET goal_refs = ARRAY[$2]::uuid[] WHERE t = $1",
        )
        .bind(t)
        .bind(Uuid::now_v7())
        .execute(pool)
        .await
        .expect_err("cooled Goal pins are sealed");
        assert!(err.to_string().contains("frozen"), "got: {err}");
        let err = sqlx::query("UPDATE proxima_core.cooled SET cold_digest = $2 WHERE t = $1")
            .bind(t)
            .bind(vec![7_u8; 32])
            .execute(pool)
            .await
            .expect_err("cooled cold digest is sealed");
        assert!(err.to_string().contains("frozen"), "got: {err}");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("cooled identity seal test failed");
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
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        apply_current_migrations(&pg).await?;
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
            "INSERT INTO proxima_core.memory (handle, t, kind, owner_id, schema_id, origins, refs, occurred_at, \"by\", via)
             VALUES ($1, $2, 'fact', $3, 'core/test-v1', '{}', '{}', uuid_extract_timestamp($2), '00000000-0000-0000-0000-000000000509'::uuid, NULL)",
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

        for query in [
            sqlx::query("UPDATE proxima_core.memory SET \"by\" = $2 WHERE t = $1"),
            sqlx::query("UPDATE proxima_core.memory SET via = $2 WHERE t = $1"),
        ] {
            let error = query.bind(t).bind(Uuid::now_v7())
                .execute(pool).await.expect_err("writer attribution is append-only");
            assert!(error.to_string().contains("append-only"), "{error}");
        }

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
        let writer: (Uuid, Option<Uuid>) = sqlx::query_as(
            "SELECT \"by\", via FROM proxima_core.memory WHERE t = $1",
        ).bind(t).fetch_one(pool).await?;
        assert_eq!(writer, (Uuid::from_u128(0x509), None));

        let delegation_id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO proxima_core.delegated_authority_grants
                (delegation_id, subject_user_id, owner_id, tool_name,
                 read_ceiling, write_ceiling, expires_at, auth_epoch, issued_at, \"by\", via)
             VALUES ($1, $2, $2, 'core_remember', 'fact', 'fact',
                     TIMESTAMPTZ '2030-01-01 00:00:00Z', 1,
                     TIMESTAMPTZ '2026-01-01 00:00:00Z', $3, $4)",
        ).bind(delegation_id).bind(owner).bind(writer.0).bind(Uuid::from_u128(0x510))
            .execute(pool).await?;
        for query in [
            sqlx::query("UPDATE proxima_core.delegated_authority_grants SET \"by\" = $2 WHERE delegation_id = $1"),
            sqlx::query("UPDATE proxima_core.delegated_authority_grants SET via = $2 WHERE delegation_id = $1"),
        ] {
            let error = query.bind(delegation_id).bind(Uuid::now_v7())
                .execute(pool).await.expect_err("delegated writer is immutable");
            assert!(error.to_string().contains("append-only"), "{error}");
        }
        let revoked = sqlx::query(
            "UPDATE proxima_core.delegated_authority_grants
                SET revoked_at = TIMESTAMPTZ '2027-01-01 00:00:00Z', revoked_by_user_id = $2
              WHERE delegation_id = $1",
        ).bind(delegation_id).bind(owner).execute(pool).await?;
        assert_eq!(revoked.rows_affected(), 1, "writer guard must preserve revocation");
        let grant_writer: (Uuid, Option<Uuid>) = sqlx::query_as(
            "SELECT \"by\", via FROM proxima_core.delegated_authority_grants WHERE delegation_id = $1",
        ).bind(delegation_id).fetch_one(pool).await?;
        assert_eq!(grant_writer, (writer.0, Some(Uuid::from_u128(0x510))));


        let err = sqlx::query(
            "INSERT INTO proxima_core.memory (handle, t, kind, owner_id, schema_id, \"by\", via)
             VALUES ($1, $3, 'abstraction', $2, 'core/test-v1', '00000000-0000-0000-0000-000000000509'::uuid, NULL)",
        )
        .bind(handle)
        .bind(owner)
        .bind(proxima_core::new_memory_uuid(1))
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

/// The final reference arrays remain append-only after the migration reset.
#[tokio::test]
async fn memory_reference_arrays_remain_append_only() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name)
        .await
        .expect("PG is required for migrations tests");
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(db_url(&db_name)).await?;
        apply_current_migrations(&pg).await?;
        let pool = pg.pool_for_tests();
        let owner = Uuid::now_v7();
        let handle = Uuid::now_v7();
        sqlx::query("INSERT INTO proxima_core.owners (owner_id, kind) VALUES ($1, 'personal')")
            .bind(owner)
            .execute(pool)
            .await?;
        sqlx::query(
            "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
             VALUES ($1, 'fact', 'core/test-v1', $2, $3)",
        )
        .bind(handle)
        .bind(owner)
        .bind(handle)
        .execute(pool)
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.memory
                 (handle, t, kind, owner_id, schema_id, refs, goal_refs, occurred_at, \"by\", via)
             VALUES ($1, $1, 'fact', $2, 'core/test-v1', '{}', '{}', uuid_extract_timestamp($1), '00000000-0000-0000-0000-000000000509'::uuid, NULL)",
        )
        .bind(handle)
        .bind(owner)
        .execute(pool)
        .await?;
        let target = Uuid::now_v7();
        for (column, query) in [
            (
                "refs",
                sqlx::query("UPDATE proxima_core.memory SET refs = ARRAY[$1]::uuid[] WHERE t = $2"),
            ),
            (
                "goal_refs",
                sqlx::query(
                    "UPDATE proxima_core.memory SET goal_refs = ARRAY[$1]::uuid[] WHERE t = $2",
                ),
            ),
        ] {
            let err = query
                .bind(target)
                .bind(handle)
                .execute(pool)
                .await
                .expect_err("reference arrays must remain append-only");
            assert!(
                err.to_string().contains("append-only"),
                "{column} refusal: {err}"
            );
        }
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("memory reference append-only test failed");
}

/// The final Fact occurrence constraint remains immutable and complete.
#[tokio::test]
async fn fact_occurred_at_is_immutable_and_required() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name)
        .await
        .expect("PG is required for migrations tests");
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(db_url(&db_name)).await?;
        apply_current_migrations(&pg).await?;
        let pool = pg.pool_for_tests();
        let owner = Uuid::now_v7();
        sqlx::query("INSERT INTO proxima_core.owners (owner_id, kind) VALUES ($1, 'personal')")
            .bind(owner)
            .execute(pool)
            .await?;
        let handle = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
             VALUES ($1, 'fact', 'core/test-v1', $2, $1)",
        )
        .bind(handle)
        .bind(owner)
        .execute(pool)
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.memory
                 (handle, t, kind, owner_id, schema_id, occurred_at, \"by\", via)
             VALUES ($1, $1, 'fact', $2, 'core/test-v1', uuid_extract_timestamp($1), '00000000-0000-0000-0000-000000000509'::uuid, NULL)",
        )
        .bind(handle)
        .bind(owner)
        .execute(pool)
        .await?;
        let refused = sqlx::query(
            "UPDATE proxima_core.memory SET occurred_at = occurred_at - interval '1 day'
              WHERE t = $1",
        )
        .bind(handle)
        .execute(pool)
        .await
        .expect_err("a Fact's occurrence time must not be rewritable");
        assert!(refused.to_string().contains("append-only"), "{refused}");

        let missing = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
             VALUES ($1, 'fact', 'core/test-v1', $2, $1)",
        )
        .bind(missing)
        .bind(owner)
        .execute(pool)
        .await?;
        let refused = sqlx::query(
            "INSERT INTO proxima_core.memory
                 (handle, t, kind, owner_id, schema_id, \"by\", via)
             VALUES ($1, $1, 'fact', $2, 'core/test-v1', '00000000-0000-0000-0000-000000000509'::uuid, NULL)",
        )
        .bind(missing)
        .bind(owner)
        .execute(pool)
        .await
        .expect_err("a Fact without an occurrence time must be refused");
        assert!(
            refused.to_string().contains("memory_fact_occurred_at_chk"),
            "{refused}"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("Fact occurrence constraint test failed");
}

/// The flavor surface is the registry for the core sidecar contract.
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn flavor_surface_is_the_registry_and_the_stamp_must_be_a_subset() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());

    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }

    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        apply_current_migrations(&pg).await?;

        let declared: Vec<String> = sqlx::query_scalar(
            "SELECT table_name FROM proxima_core.flavor_surface
              WHERE flavor_id = 'core' ORDER BY table_name",
        )
        .fetch_all(pg.pool_for_tests())
        .await?;

        let mut contract: Vec<String> = proxima_core::FLAVOR_0
            .schemas
            .iter()
            .filter_map(|schema| schema.sidecar_table)
            .map(str::to_owned)
            .collect();
        contract.sort();
        contract.dedup();

        assert_eq!(
            declared, contract,
            "the migration's flavor #0 rows are the contract's sidecar surfaces"
        );

        let owner_id = Uuid::now_v7();
        let handle = Uuid::now_v7();
        sqlx::query("INSERT INTO proxima_core.owners (owner_id, kind) VALUES ($1, 'personal')")
            .bind(owner_id)
            .execute(pg.pool_for_tests())
            .await?;
        sqlx::query(
            "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
             VALUES ($1, 'fact', 'core/agent-note-v1', $2, $3)",
        )
        .bind(handle)
        .bind(owner_id)
        .bind(handle)
        .execute(pg.pool_for_tests())
        .await?;

        let err = sqlx::query(
            "INSERT INTO proxima_core.memory
                 (handle, t, kind, owner_id, schema_id, sidecar_tables, occurred_at, \"by\", via)
             VALUES ($1, $1, 'fact', $2, 'core/agent-note-v1',
                     ARRAY['proxima_core.not_a_declared_surface'], uuid_extract_timestamp($1), '00000000-0000-0000-0000-000000000509'::uuid, NULL)",
        )
        .bind(handle)
        .bind(owner_id)
        .execute(pg.pool_for_tests())
        .await
        .expect_err("an undeclared sidecar stamp must be refused");
        assert!(
            err.to_string().contains("not_a_declared_surface"),
            "the refusal names the offending element: {err}"
        );

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("flavor_surface subset test failed");
}

/// The migration text IS the generator's output.
///
/// The generator has two consumers (`crates/storage-pg/src/projection.rs`):
/// the migration author, who pastes its output into the baselines, and the
/// boot guardrail, which re-runs it. This is what keeps the first honest —
/// a hand edit to either baseline's projection block, or a change to the
/// generator that the baselines did not follow, fails here rather than at
/// the next boot.
#[test]
fn generator_output_is_the_migration_text() {
    let core = proxima_storage_pg::projection::projection_artifacts(&proxima_core::FLAVOR_0)
        .expect("core artifacts")
        .expect("flavor #0 declares a projection");
    let baseline = include_str!("../migrations/0033_baseline.sql");
    for statement in [core.table.forward.as_str(), core.index.forward.as_str()] {
        assert!(
            baseline.contains(statement),
            "0033_baseline.sql does not carry the generator's output verbatim:\n{statement}"
        );
    }
    let definitions = include_str!("../definitions/triggers.sql");
    assert!(
        definitions.contains(&core.language_trigger.forward),
        "definitions/triggers.sql does not carry the projection trigger"
    );

    // A flavor's baseline is checked from its own crate's tests (the
    // example flavor's `tests/migration_pg.rs`), which is where that file is.
    assert!(
        baseline.contains(proxima_storage_pg::projection::BTREE_GIN_EXTENSION),
        "the composite GIN needs btree_gin"
    );
}

/// One frozen core registry, for the tests below and nothing else.
fn frozen_core_sidecars() -> proxima_storage_pg::PgSidecarRegistryFrozen {
    let registry = proxima_core::FlavorRegistry::new().freeze_or_panic_for_tests();
    let mut pg = proxima_storage_pg::PgSidecarRegistry::new();
    proxima_storage_pg::register_core_pg_sidecars(&mut pg);
    pg.freeze_against(&registry)
        .expect("the core registration freezes against the core contract")
}

/// The definitions text IS the declaration-integrity generator's output.
///
/// The same pin as `generator_output_is_the_migration_text`, one layer
/// down: `crates/storage-pg/src/integrity.rs` has the same two consumers —
/// the definitions author, who pastes its output into the definitions, and
/// `ensure_declaration_triggers`, which re-runs it against the catalog at
/// boot. A hand edit to the trigger block, or a generator change the
/// migration did not follow, fails here rather than at the next boot.
///
/// The text lives in `definitions/triggers.sql` alongside the other core
/// trigger definitions.
#[test]
fn generated_declaration_triggers_are_the_migration_text() {
    use proxima_storage_pg::integrity::DECLARATION_TRIGGER_FUNCTION;

    let migration = include_str!("../definitions/triggers.sql");
    let functions = include_str!("../definitions/functions.sql");
    assert!(
        functions.contains(DECLARATION_TRIGGER_FUNCTION),
        "definitions/functions.sql does not carry the shared trigger function \
         verbatim:\n{DECLARATION_TRIGGER_FUNCTION}"
    );
    assert!(
        functions.contains("CREATE OR REPLACE FUNCTION"),
        "definitions/functions.sql carries its trigger functions"
    );

    let artifacts = frozen_core_sidecars()
        .declaration_trigger_artifacts("core")
        .expect("core's declaration triggers");
    assert_eq!(
        artifacts.len(),
        6,
        "the retired call sidecar no longer receives a declaration trigger"
    );
    for artifact in &artifacts {
        assert!(
            migration.contains(&artifact.forward),
            "definitions/triggers.sql does not carry the generator's output verbatim:\n{}",
            artifact.forward
        );
    }
}

/// The presence-lane migration text IS the generator's output.
///
/// The sibling of `generated_declaration_triggers_are_the_migration_text`,
/// one lane later. Split from it rather than folded into it because
/// `definitions/triggers.sql` is applied in live databases: a
/// generator whose existing output grew would make that file's pin
/// unsatisfiable, so the new families get a new generator, a new file and a
/// new pin.
///
/// The count assertions are the ones that make this a pin rather than a
/// smoke test: `mcp_call_logged_v1` is owner-pinned and gets the UPDATE
/// guard but NOT a presence trigger, so a change that started guarding it —
/// or that stopped guarding one of the other five — fails here.
#[test]
fn generated_presence_triggers_are_the_migration_text() {
    use proxima_storage_pg::integrity::{DELETE_GUARD_FUNCTION, PRESENCE_TRIGGER_FUNCTION};

    let migration = include_str!("../definitions/triggers.sql");
    let functions = include_str!("../definitions/functions.sql");
    for function in [PRESENCE_TRIGGER_FUNCTION, DELETE_GUARD_FUNCTION] {
        assert!(
            functions.contains(function),
            "definitions/functions.sql does not carry a shared function \
             verbatim:\n{function}"
        );
    }
    for name in [
        "assert_declared_sidecar_present",
        "assert_row_not_still_declared",
    ] {
        assert!(
            functions.contains(name),
            "definitions/functions.sql declares the shared function"
        );
    }

    let artifacts = frozen_core_sidecars()
        .presence_trigger_artifacts("core")
        .expect("core's presence triggers");
    let presence = artifacts
        .iter()
        .filter(|artifact| {
            artifact
                .forward
                .contains("AFTER INSERT OR UPDATE OF sidecar_tables")
        })
        .count();
    let orphan = artifacts
        .iter()
        .filter(|artifact| artifact.forward.contains("AFTER DELETE ON"))
        .count();
    let repoint = artifacts
        .iter()
        .filter(|artifact| artifact.forward.contains("BEFORE UPDATE OF"))
        .count();
    assert_eq!(
        (presence, orphan),
        (6, 6),
        "flavor #0 registers seven memory sidecars, one of them owner-pinned; only the other \
         six get a presence trigger and an orphan guard"
    );
    assert_eq!(
        repoint, 6,
        "every surviving registered memory sidecar gets the UPDATE-of-key guard"
    );
    assert_eq!(
        artifacts.len(),
        presence + orphan + repoint,
        "every generated artifact belongs to one of the three families"
    );
    for artifact in &artifacts {
        assert!(
            migration.contains(&artifact.forward),
            "definitions/triggers.sql does not carry the generator's output verbatim:\n{}",
            artifact.forward
        );
    }
    assert!(
        !migration.contains("mcp_call_logged_v1', 't')"),
        "the owner-pinned sidecar must get neither a presence trigger nor an orphan guard: \
         its rows are erased on their own owner's schedule, so its stamp records a past \
         write rather than claiming a present row"
    );
}

/// `SQLx` records each migration as SHA-384 of its file minus the
/// `ignored-chars` of `crates/storage-pg/sqlx.toml` (`\r`), and every deployed
/// ledger holds exactly that. `scripts/check-migrations.py` pins the
/// released file bytes; this pins the computation over every embedded file,
/// so an `ignored-chars` edit or an `SQLx` upgrade that hashes differently —
/// which would change every recorded checksum at once — fails here.
#[test]
fn embedded_core_checksums_are_sha384_of_the_files_without_carriage_returns() {
    use sha2::{Digest, Sha384};

    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    let migrator = proxima_storage_pg::core_migrator();
    let mut checked = 0;
    for entry in std::fs::read_dir(&directory).expect("core migrations directory") {
        let path = entry.expect("migration entry").path();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        let Some(version) = name
            .strip_suffix(".sql")
            .and_then(|stem| stem.split('_').next())
            .and_then(|version| version.parse::<i64>().ok())
        else {
            continue;
        };
        let bytes: Vec<u8> = std::fs::read(&path)
            .expect("readable migration")
            .into_iter()
            .filter(|byte| *byte != b'\r')
            .collect();
        let embedded = migrator
            .iter()
            .find(|migration| migration.version == version)
            .unwrap_or_else(|| panic!("{name} is not embedded"));
        assert_eq!(
            embedded.checksum.as_ref(),
            &Sha384::digest(&bytes)[..],
            "{name}: SQLx checksum computation changed"
        );
        checked += 1;
    }
    assert_eq!(
        checked,
        migrator.iter().count(),
        "every embedded migration checked"
    );
}

/// The invariant, through a raw connection that has bypassed every line of
/// Rust in this workspace.
///
/// This is the whole point of putting it in the database: the memory row
/// exists and is legal, the sidecar row's FK to it is satisfied, and the
/// only thing wrong is that the memory never declared the table. A port
/// check cannot see this write at all.
#[tokio::test]
async fn a_sidecar_row_no_memory_declares_is_refused_by_the_database() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        apply_current_migrations(&pg).await?;
        let pool = pg.pool_for_tests();

        let owner_id = Uuid::now_v7();
        sqlx::query("INSERT INTO proxima_core.owners (owner_id, kind) VALUES ($1, 'personal')")
            .bind(owner_id)
            .execute(pool)
            .await?;

        // Two memories of the same schema. One declares the note sidecar,
        // one declares nothing — so the ONLY difference between the two
        // inserts below is the stamp. The declaring one gets its row in the
        // same transaction, because the other direction of the invariant
        // (`assert_declared_sidecar_present`) refuses a stamp that never
        // gets its row.
        let mut ids = Vec::new();
        for declared in [false, true] {
            let handle = Uuid::now_v7();
            let mut stamped = pool.begin().await?;
            sqlx::query(
                "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
                 VALUES ($1, 'fact', 'core/agent-note-v1', $2, $1)",
            )
            .bind(handle)
            .bind(owner_id)
            .execute(&mut *stamped)
            .await?;
            let stamp: Vec<String> = if declared {
                vec!["proxima_core.agent_note_v1".to_owned()]
            } else {
                Vec::new()
            };
            sqlx::query(
                "INSERT INTO proxima_core.memory
                     (handle, t, kind, owner_id, schema_id, sidecar_tables, occurred_at, \"by\", via)
                 VALUES ($1, $1, 'fact', $2, 'core/agent-note-v1', $3, uuid_extract_timestamp($1), '00000000-0000-0000-0000-000000000509'::uuid, NULL)",
            )
            .bind(handle)
            .bind(owner_id)
            .bind(&stamp)
            .execute(&mut *stamped)
            .await?;
            if declared {
                // The same statement the undeclared memory is refused below,
                // against the memory that declared it.
                sqlx::query(
                    "INSERT INTO proxima_core.agent_note_v1 (t, note_id, title, body, tags)
                     VALUES ($1, $1, 'title', 'body', '{}')",
                )
                .bind(handle)
                .execute(&mut *stamped)
                .await
                .expect("a declared sidecar row is admitted");
            }
            stamped.commit().await?;
            ids.push(handle);
        }

        let err = sqlx::query(
            "INSERT INTO proxima_core.agent_note_v1 (t, note_id, title, body, tags)
             VALUES ($1, $1, 'title', 'body', '{}')",
        )
        .bind(ids[0])
        .execute(pool)
        .await
        .expect_err("an undeclared sidecar row must be refused by the database");
        let message = err.to_string();
        assert!(
            message.contains("proxima_core.agent_note_v1"),
            "the refusal names the table: {message}"
        );
        assert!(
            message.contains(&ids[0].to_string()),
            "the refusal names the memory row: {message}"
        );

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("declaration integrity trigger test failed");
}

/// Final Goal replay declarations retain their key, checks, and cascade.
#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one Goal fixture exercises each declaration refusal and its cascade"
)]
async fn goal_replay_declaration_enforces_identity_and_cascade() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name)
        .await
        .expect("PG is required for migrations tests");
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(db_url(&db_name)).await?;
        apply_current_migrations(&pg).await?;
        let pool = pg.pool_for_tests();
        let owner = Uuid::now_v7();
        let handle = Uuid::now_v7();
        sqlx::query("INSERT INTO proxima_core.owners (owner_id, kind) VALUES ($1, 'personal')")
            .bind(owner)
            .execute(pool)
            .await?;
        sqlx::query(
            "INSERT INTO proxima_core.goal_head (handle, schema_id, owner_id, t)
             VALUES ($1, 'core/goal-v1', $2, $3)",
        )
        .bind(handle)
        .bind(owner)
        .bind(handle)
        .execute(pool)
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.goal
                 (handle, t, owner_id, title, state, request_id, \"by\", via)
             VALUES ($1, $1, $2, 'replay', 'Active', 'replay-test', '00000000-0000-0000-0000-000000000509'::uuid, NULL)",
        )
        .bind(handle)
        .bind(owner)
        .execute(pool)
        .await?;
        let err = sqlx::query(
            "INSERT INTO proxima_core.goal_replay_declaration (goal_t, declaration, edge_count)
             VALUES ($1, '[]'::jsonb, 0)",
        )
        .bind(handle)
        .execute(pool)
        .await
        .expect_err("object check must refuse arrays");
        assert_eq!(
            err.as_database_error()
                .and_then(sqlx::error::DatabaseError::code),
            Some("23514".into()),
            "{err}"
        );
        assert!(
            err.to_string()
                .contains("goal_replay_declaration_object_chk"),
            "{err}"
        );
        let err = sqlx::query(
            "INSERT INTO proxima_core.goal_replay_declaration (goal_t, declaration, edge_count)
             VALUES ($1, '{}'::jsonb, -1)",
        )
        .bind(handle)
        .execute(pool)
        .await
        .expect_err("edge count check must refuse negatives");
        assert_eq!(
            err.as_database_error()
                .and_then(sqlx::error::DatabaseError::code),
            Some("23514".into()),
            "{err}"
        );
        assert!(
            err.to_string().contains("goal_replay_edge_count_chk"),
            "{err}"
        );
        sqlx::query(
            "INSERT INTO proxima_core.goal_replay_declaration (goal_t, declaration, edge_count)
             VALUES ($1, '{}'::jsonb, 0)",
        )
        .bind(handle)
        .execute(pool)
        .await?;
        let err = sqlx::query(
            "INSERT INTO proxima_core.goal_replay_declaration (goal_t, declaration, edge_count)
             VALUES ($1, '{}'::jsonb, 0)",
        )
        .bind(handle)
        .execute(pool)
        .await
        .expect_err("goal_t is the primary key");
        assert_eq!(
            err.as_database_error()
                .and_then(sqlx::error::DatabaseError::code),
            Some("23505".into()),
            "{err}"
        );
        assert!(
            err.to_string().contains("goal_replay_declaration_pkey"),
            "{err}"
        );
        let missing_goal = Uuid::now_v7();
        let err = sqlx::query(
            "INSERT INTO proxima_core.goal_replay_declaration (goal_t, declaration, edge_count)
             VALUES ($1, '{}'::jsonb, 0)",
        )
        .bind(missing_goal)
        .execute(pool)
        .await
        .expect_err("a replay declaration must reference an existing Goal");
        assert_eq!(
            err.as_database_error()
                .and_then(sqlx::error::DatabaseError::code),
            Some("23503".into()),
            "{err}"
        );
        assert!(
            err.to_string()
                .contains("goal_replay_declaration_goal_t_fkey"),
            "{err}"
        );
        sqlx::query("DELETE FROM proxima_core.goal WHERE t = $1")
            .bind(handle)
            .execute(pool)
            .await?;
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM proxima_core.goal_replay_declaration WHERE goal_t = $1",
        )
        .bind(handle)
        .fetch_one(pool)
        .await?;
        assert_eq!(
            count, 0,
            "the Goal foreign key cascades declaration removal"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("Goal replay declaration constraint test failed");
}

/// The checks of `goal_change_v1` refuse the rows they name.
#[tokio::test]
async fn goal_change_checks_refuse_the_rows_they_name() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(db_url(&db_name)).await?;
        apply_current_migrations(&pg).await?;
        let pool = pg.pool_for_tests();
        // kind, has parent, pending, revision, the `goal_change_v1_<>_chk` that refuses it
        let rows = [
            ("pause", false, Some("keep"), 1, None),
            ("set", false, Some("keep"), 1, Some("pending")),
            ("resume", false, Some("discard"), 1, Some("pending")),
            ("pause", false, None, 1, Some("pending")),
            ("set", true, None, 1, Some("parent")),
            ("decompose_child", false, None, 1, Some("parent")),
            ("set", false, None, 0, Some("revision")),
        ];
        for (kind, has_parent, pending, revision, refused_by) in rows {
            // Only the checks are under test, not the declaration triggers.
            let mut tx = pool.begin().await?;
            sqlx::query("SET LOCAL session_replication_role = replica")
                .execute(&mut *tx)
                .await?;
            let inserted = sqlx::query(
                "INSERT INTO proxima_core.goal_change_v1
                     (t, series, goal_t, revision, state, change_kind, parent_t, pending)
                 VALUES (gen_random_uuid(), gen_random_uuid(), gen_random_uuid(), $1, 'Paused',
                         $2::proxima_core.goal_change_kind,
                         CASE WHEN $3 THEN gen_random_uuid() END,
                         $4::proxima_core.goal_pending_work)",
            )
            .bind(revision)
            .bind(kind)
            .bind(has_parent)
            .bind(pending)
            .execute(&mut *tx)
            .await;
            let refused = inserted
                .err()
                .and_then(|error| error.as_database_error()?.constraint().map(str::to_owned));
            let wanted = refused_by.map(|check| format!("goal_change_v1_{check}_chk"));
            assert_eq!(refused, wanted, "{kind} {pending:?} {revision}");
            if refused.is_none() {
                tx.commit().await?;
            }
        }

        let kept: i64 = sqlx::query_scalar("SELECT count(*) FROM proxima_core.goal_change_v1")
            .fetch_one(pool)
            .await?;
        assert_eq!(kept, 1);
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("goal_change_v1 check test failed");
}

/// The boot guardrail is bidirectional, like its projection sibling.
#[tokio::test]
async fn a_dropped_declaration_trigger_fails_the_boot_guardrail() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        apply_current_migrations(&pg).await?;
        let pool = pg.pool_for_tests();
        let sidecars = frozen_core_sidecars();

        proxima_storage_pg::integrity::ensure_declaration_triggers(pool, &sidecars)
            .await
            .expect("a freshly migrated database satisfies the guardrail");

        sqlx::query("DROP TRIGGER agent_note_v1_declared_by_memory ON proxima_core.agent_note_v1")
            .execute(pool)
            .await?;
        let err = proxima_storage_pg::integrity::ensure_declaration_triggers(pool, &sidecars)
            .await
            .expect_err("a registered sidecar with no trigger is unguarded");
        let message = err.to_string();
        assert!(
            message.contains("proxima_core.agent_note_v1"),
            "the refusal names the table that lost its guard: {message}"
        );

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("declaration trigger boot guardrail test failed");
}

/// The same guardrail, over the two trigger families in `definitions/triggers.sql`.
///
/// A dropped presence trigger is a table whose `stamp ⊆ rows` direction is
/// unenforced; a presence trigger with the right NAME and the wrong
/// ARGUMENTS is worse, because it looks installed and guards a different
/// table. The guardrail has to refuse both, so both are made here.
#[tokio::test]
async fn a_damaged_presence_trigger_fails_the_boot_guardrail() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        apply_current_migrations(&pg).await?;
        let pool = pg.pool_for_tests();
        let sidecars = frozen_core_sidecars();

        proxima_storage_pg::integrity::ensure_declaration_triggers(pool, &sidecars)
            .await
            .expect("a freshly migrated database satisfies the guardrail");

        // 1. Gone — either end of the direction.
        for (why, drop) in [
            (
                "no presence trigger",
                "DROP TRIGGER memory_declares_proxima_core_agent_note_v1
                     ON proxima_core.memory",
            ),
            (
                "no orphan guard",
                "DROP TRIGGER agent_note_v1_declared_by_memory_on_delete
                     ON proxima_core.agent_note_v1",
            ),
        ] {
            // SQL-POLICY: fixed-fragment — a literal from the table above.
            sqlx::query(sqlx::AssertSqlSafe(drop)).execute(pool).await?;
            let err = proxima_storage_pg::integrity::ensure_declaration_triggers(pool, &sidecars)
                .await
                .expect_err(why)
                .to_string();
            assert!(
                err.contains("proxima_core.agent_note_v1"),
                "the refusal names the table that lost its guard ({why}): {err}"
            );
            // Put it back, so the next case is the only damage in the
            // database and the assertion above cannot pass for the wrong
            // reason.
            let sidecars_for_repair = sidecars.presence_trigger_artifacts("core")?;
            for artifact in &sidecars_for_repair {
                // SQL-POLICY: generated
                sqlx::raw_sql(sqlx::AssertSqlSafe(artifact.forward.clone()))
                    .execute(pool)
                    .await?;
            }
            proxima_storage_pg::integrity::ensure_declaration_triggers(pool, &sidecars)
                .await
                .expect("re-applying the generator's output repairs it");
        }

        // 2. Present, correctly named, and wrong in a way the argument list
        //    does not show. Each of these passes a check that looks only for
        //    the trigger's name and its `EXECUTE FUNCTION …(args)`:
        //
        //    - a `WHEN` that never matches disarms the guard completely,
        //      which is the exact hole this direction exists to close;
        //    - no `WHEN` at all fires it on every memory insert (safe only
        //      because the function body re-tests membership, and a guard
        //      whose safety depends on that is not the generated one);
        //    - not `DEFERRABLE` refuses every legitimate write;
        //    - the wrong surface guards a table nobody asked about.
        for (why, when, deferral, args) in [
            (
                "a WHEN that never matches",
                "WHEN (false)",
                "DEFERRABLE INITIALLY DEFERRED",
                "'proxima_core.agent_note_v1', 't'",
            ),
            (
                "no WHEN at all",
                "",
                "DEFERRABLE INITIALLY DEFERRED",
                "'proxima_core.agent_note_v1', 't'",
            ),
            (
                "an immediate trigger",
                "WHEN ('proxima_core.agent_note_v1' = ANY (NEW.sidecar_tables))",
                "",
                "'proxima_core.agent_note_v1', 't'",
            ),
            (
                "the wrong guarded surface",
                "WHEN ('proxima_core.agent_note_v1' = ANY (NEW.sidecar_tables))",
                "DEFERRABLE INITIALLY DEFERRED",
                "'proxima_core.utterance_v1', 't'",
            ),
        ] {
            let ddl = format!(
                // SQL-POLICY: fixed-fragment — see the comment above.
                "DROP TRIGGER IF EXISTS memory_declares_proxima_core_agent_note_v1
                     ON proxima_core.memory;
                 CREATE CONSTRAINT TRIGGER memory_declares_proxima_core_agent_note_v1
                     AFTER INSERT OR UPDATE OF sidecar_tables ON proxima_core.memory
                     {deferral}
                     FOR EACH ROW
                     {when}
                     EXECUTE FUNCTION \
                         proxima_core.assert_declared_sidecar_present({args})"
            );
            // SQL-POLICY: fixed-fragment — every part comes from the fixed
            // table of damaged shapes above.
            sqlx::raw_sql(sqlx::AssertSqlSafe(ddl))
                .execute(pool)
                .await?;
            let err = proxima_storage_pg::integrity::ensure_declaration_triggers(pool, &sidecars)
                .await
                .expect_err(why)
                .to_string();
            assert!(
                err.contains("proxima_core.agent_note_v1"),
                "the refusal names the table whose guard is wrong ({why}): {err}"
            );
        }

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("presence trigger boot guardrail test failed");
}

/// The `row ⊆ stamp` direction is enforced by the current definitions bundle:
/// a legal row could
/// be repointed onto a memory that declares nothing, which is the same
/// undeclared row the INSERT guard exists to refuse.
///
/// Every sidecar core ships is append-only, so a core table would prove
/// nothing here — its own append-only trigger refuses the UPDATE first, and
/// a guard that was never installed would pass. The table is made by hand
/// and given the generated trigger, so the refusal under test is the only
/// one that can fire.
#[tokio::test]
async fn a_key_repoint_onto_an_undeclared_memory_is_refused() {
    const TABLE: &str = "public.repointable_sidecar_v1";
    const KEY: &str = "t";

    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        apply_current_migrations(&pg).await?;
        let pool = pg.pool_for_tests();

        sqlx::query(
            "CREATE TABLE public.repointable_sidecar_v1 (
                 t uuid PRIMARY KEY REFERENCES proxima_core.memory (t),
                 body text NOT NULL
             )",
        )
        .execute(pool)
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.flavor_surface (table_name, flavor_id)
             VALUES ($1, 'repoint-test')",
        )
        .bind(TABLE)
        .execute(pool)
        .await?;
        for artifact in [
            proxima_storage_pg::integrity::declaration_trigger(TABLE, KEY)
                .expect("the generator emits the INSERT guard"),
            proxima_storage_pg::integrity::key_repoint_trigger(TABLE, KEY)
                .expect("the generator emits the UPDATE guard"),
        ] {
            // SQL-POLICY: generated
            sqlx::query(sqlx::AssertSqlSafe(artifact.forward))
                .execute(pool)
                .await?;
        }

        let owner_id = Uuid::now_v7();
        sqlx::query("INSERT INTO proxima_core.owners (owner_id, kind) VALUES ($1, 'personal')")
            .bind(owner_id)
            .execute(pool)
            .await?;

        // One memory that declares the table, one that declares nothing —
        // the repoint target.
        let declared = Uuid::now_v7();
        let bare = Uuid::now_v7();
        for (t, stamp) in [(declared, vec![TABLE.to_owned()]), (bare, Vec::new())] {
            sqlx::query(
                "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
                 VALUES ($1, 'fact', 'core/agent-note-v1', $2, $1)",
            )
            .bind(t)
            .bind(owner_id)
            .execute(pool)
            .await?;
            sqlx::query(
                "INSERT INTO proxima_core.memory
                     (handle, t, kind, owner_id, schema_id, sidecar_tables, occurred_at, \"by\", via)
                 VALUES ($1, $1, 'fact', $2, 'core/agent-note-v1', $3, uuid_extract_timestamp($1), '00000000-0000-0000-0000-000000000509'::uuid, NULL)",
            )
            .bind(t)
            .bind(owner_id)
            .bind(&stamp)
            .execute(pool)
            .await?;
        }
        sqlx::query("INSERT INTO public.repointable_sidecar_v1 (t, body) VALUES ($1, 'body')")
            .bind(declared)
            .execute(pool)
            .await?;

        let err = sqlx::query("UPDATE public.repointable_sidecar_v1 SET t = $2 WHERE t = $1")
            .bind(declared)
            .bind(bare)
            .execute(pool)
            .await
            .expect_err("a repoint onto a memory that declares nothing must be refused");
        let message = err.to_string();
        assert!(
            message.contains(TABLE),
            "the refusal names the table: {message}"
        );
        assert!(
            message.contains(&bare.to_string()),
            "…and the memory it would have been repointed onto: {message}"
        );

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("key repoint guard test failed");
}

/// The boot guardrail must not care what session settings the pool arrives
/// with.
///
/// Two of them change what `pg_get_triggerdef` renders. `search_path` omits
/// the schema of anything the path already resolves, so a connection with
/// `proxima_core` on its path renders `EXECUTE FUNCTION
/// assert_row_not_still_declared(...)`; `quote_all_identifiers` double-quotes
/// every identifier in the definition, down to `("new"."sidecar_tables")`
/// inside a `WHEN`. Right trigger, different string — and compared naively, a
/// correctly migrated database refuses to boot. Both are settable
/// per-database, which is how this test sets them. The guardrail pins both
/// for its own read; this is what says so.
#[tokio::test]
async fn the_boot_guardrail_ignores_the_callers_session_settings() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        apply_current_migrations(&pg).await?;
        let pool = pg.pool_for_tests();
        let sidecars = frozen_core_sidecars();

        // The path that would break a naive comparison, made the default for
        // every connection this database hands out — so the guardrail cannot
        // dodge it by picking a fresh one from the pool.
        for setting in [
            "search_path TO proxima_core, public",
            "quote_all_identifiers TO on",
        ] {
            // SQL-POLICY: fixed-fragment — the only interpolations are the
            // uuid-derived database name this test created and a literal from
            // the list above.
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "ALTER DATABASE {db_name} SET {setting}"
            )))
            .execute(pool)
            .await?;
        }

        let (runtime_url, _) = proxima_pg_testkit::split_role_urls(&db_name).await?;
        let scoped = PgStorage::connect_with_config(
            &runtime_url,
            proxima_storage_pg::PgPoolConfig::default(),
            proxima_storage_pg::PgTuning::default(),
            pg.platform_scope_for_host(),
            1,
        )
        .await?;
        let settings = || async {
            let path: String = sqlx::query_scalar("SHOW search_path")
                .fetch_one(scoped.pool_for_tests())
                .await?;
            let quoting: String = sqlx::query_scalar("SHOW quote_all_identifiers")
                .fetch_one(scoped.pool_for_tests())
                .await?;
            Ok::<_, sqlx::Error>((path, quoting))
        };
        let before = settings().await?;
        assert_eq!(
            (before.0.as_str(), before.1.as_str()),
            ("proxima_core, public", "on"),
            "the fixture must actually change the settings it claims to"
        );

        proxima_storage_pg::integrity::ensure_declaration_triggers(
            scoped.pool_for_tests(),
            &sidecars,
        )
        .await
        .map_err(|err| format!("a correctly migrated database must boot on any path: {err}"))?;

        // And the pins are not left behind on the pooled connection.
        let after = settings().await?;
        assert_eq!(
            after, before,
            "the guardrail leaves the caller's settings as it found them"
        );

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("session-setting-independent boot guardrail test failed");
}

/// The boot guardrail compares whole rendered trigger definitions, so it has
/// to agree with `pg_get_triggerdef` on every family AND on a key column that
/// is not `t`.
///
/// Nothing else can catch this. Every memory sidecar the tree ships happens to
/// key on `t`, so a rendering the guardrail gets wrong for any other column —
/// and the key appears in the `BEFORE UPDATE OF <key>` clause and in two
/// argument lists — would pass every other test here and then refuse to boot
/// the first out-of-tree flavor that used `KeyShape::MemoryT { column }`. The
/// table is made by hand, registered, given the generator's own four
/// artifacts, and put in front of the real guardrail.
#[tokio::test]
async fn the_boot_guardrail_accepts_a_sidecar_keyed_on_something_other_than_t() {
    const TABLE: &str = "public.oddly_keyed_sidecar_v1";
    const KEY: &str = "note_memory_id";

    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        apply_current_migrations(&pg).await?;
        let pool = pg.pool_for_tests();

        sqlx::query(
            "CREATE TABLE public.oddly_keyed_sidecar_v1 (
                 note_memory_id uuid PRIMARY KEY REFERENCES proxima_core.memory (t),
                 body text NOT NULL
             )",
        )
        .execute(pool)
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.flavor_surface (table_name, flavor_id)
             VALUES ($1, 'odd-key-test')",
        )
        .bind(TABLE)
        .execute(pool)
        .await?;

        let sidecars =
            frozen_core_sidecars().with_memory_sidecar_for_tests("core/odd-key-v1", TABLE, KEY);

        // Without its triggers the guardrail must refuse: otherwise the
        // acceptance below would prove only that the table was ignored.
        let err = proxima_storage_pg::integrity::ensure_declaration_triggers(pool, &sidecars)
            .await
            .expect_err("a registered sidecar with no triggers is unguarded");
        assert!(
            err.to_string().contains(TABLE),
            "the refusal names the unguarded table: {err}"
        );

        for artifact in [
            proxima_storage_pg::integrity::declaration_trigger(TABLE, KEY)?,
            proxima_storage_pg::integrity::key_repoint_trigger(TABLE, KEY)?,
            proxima_storage_pg::integrity::presence_trigger(TABLE, KEY)?,
            proxima_storage_pg::integrity::delete_guard_trigger(TABLE, KEY)?,
        ] {
            // SQL-POLICY: generated
            sqlx::raw_sql(sqlx::AssertSqlSafe(artifact.forward))
                .execute(pool)
                .await?;
        }

        proxima_storage_pg::integrity::ensure_declaration_triggers(pool, &sidecars)
            .await
            .map_err(|err| {
                format!(
                    "the guardrail must accept what its own generators emit, on any declared \
                     key column: {err}"
                )
            })?;

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("non-`t` key boot guardrail test failed");
}

/// The guard reads the column the REGISTRATION declares, proven against a
/// live table keyed on something other than `t`.
///
/// The unit test on the emitted text cannot show this: `to_jsonb(NEW) ->>
/// TG_ARGV[0]` is what makes one shared function serve every table, and a
/// hardcoded `NEW.t` would pass every other test in this file, because
/// every sidecar core ships happens to key on `t`. Here the column is
/// `note_memory_id`, so a guard that spelled `t` would read NULL and refuse
/// the admitted row as well as the undeclared one.
#[tokio::test]
async fn the_trigger_reads_the_declared_key_column_of_a_renamed_sidecar() {
    const TABLE: &str = "public.renamed_sidecar_v1";
    const KEY: &str = "note_memory_id";

    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        apply_current_migrations(&pg).await?;
        let pool = pg.pool_for_tests();

        sqlx::query(
            "CREATE TABLE public.renamed_sidecar_v1 (
                 note_memory_id uuid PRIMARY KEY REFERENCES proxima_core.memory (t),
                 body text NOT NULL
             )",
        )
        .execute(pool)
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.flavor_surface (table_name, flavor_id)
             VALUES ($1, 'renamed-test')",
        )
        .bind(TABLE)
        .execute(pool)
        .await?;
        let artifact = proxima_storage_pg::integrity::declaration_trigger(TABLE, KEY)
            .expect("the generator emits a trigger for a renamed key");
        // SQL-POLICY: generated
        sqlx::query(sqlx::AssertSqlSafe(artifact.forward))
            .execute(pool)
            .await?;

        let owner_id = Uuid::now_v7();
        sqlx::query("INSERT INTO proxima_core.owners (owner_id, kind) VALUES ($1, 'personal')")
            .bind(owner_id)
            .execute(pool)
            .await?;
        let mut ids = Vec::new();
        for declared in [false, true] {
            let handle = Uuid::now_v7();
            sqlx::query(
                "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
                 VALUES ($1, 'fact', 'core/agent-note-v1', $2, $1)",
            )
            .bind(handle)
            .bind(owner_id)
            .execute(pool)
            .await?;
            let stamp: Vec<String> = if declared {
                vec![TABLE.to_owned()]
            } else {
                Vec::new()
            };
            sqlx::query(
                "INSERT INTO proxima_core.memory
                     (handle, t, kind, owner_id, schema_id, sidecar_tables, occurred_at, \"by\", via)
                 VALUES ($1, $1, 'fact', $2, 'core/agent-note-v1', $3, uuid_extract_timestamp($1), '00000000-0000-0000-0000-000000000509'::uuid, NULL)",
            )
            .bind(handle)
            .bind(owner_id)
            .bind(&stamp)
            .execute(pool)
            .await?;
            ids.push(handle);
        }

        let err = sqlx::query(
            "INSERT INTO public.renamed_sidecar_v1 (note_memory_id, body) VALUES ($1, 'body')",
        )
        .bind(ids[0])
        .execute(pool)
        .await
        .expect_err("an undeclared row is refused on the declared key column");
        assert!(
            err.to_string().contains(TABLE) && err.to_string().contains(&ids[0].to_string()),
            "the refusal names the table and the memory it read off {KEY}: {err}"
        );

        sqlx::query(
            "INSERT INTO public.renamed_sidecar_v1 (note_memory_id, body) VALUES ($1, 'body')",
        )
        .bind(ids[1])
        .execute(pool)
        .await
        .expect("a declared row is admitted, so the guard read the right column");

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("renamed-key declaration trigger test failed");
}

/// A membership names a group: the database refuses a user id or an
/// unregistered id in the group position.
#[tokio::test]
async fn the_database_refuses_a_membership_under_a_users_id()
-> Result<(), Box<dyn std::error::Error>> {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await?;
    let result = async {
        let pg = PgStorage::connect_for_tests(db_url(&db_name)).await?;
        apply_current_migrations(&pg).await?;
        let pool = pg.pool_for_tests();
        let (user, member) = (Uuid::now_v7(), Uuid::now_v7());
        sqlx::query("INSERT INTO proxima_core.owners (owner_id, kind) VALUES ($1, 'personal')")
            .bind(user)
            .execute(pool)
            .await?;
        for group_id in [user, Uuid::now_v7()] {
            let err = sqlx::query(
                "INSERT INTO proxima_core.group_memberships (group_id, member_user_id, relation)
                 VALUES ($1, $2, 'viewer')",
            )
            .bind(group_id)
            .bind(member)
            .execute(pool)
            .await
            .expect_err("a membership's group must be a registered group");
            assert!(
                err.to_string()
                    .contains("group_memberships_group_registered_fk"),
                "{err}"
            );
        }
        let err = sqlx::query(
            "INSERT INTO proxima_core.group_memberships
                 (group_id, member_user_id, relation, group_kind)
             VALUES ($1, $2, 'viewer', 'personal')",
        )
        .bind(user)
        .bind(member)
        .execute(pool)
        .await
        .expect_err("the kind column cannot be spelled around");
        assert!(
            err.to_string().contains("group_memberships_group_kind_chk"),
            "{err}"
        );
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn publication_record_state_rejects_claimed() -> Result<(), Box<dyn std::error::Error>> {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await?;
    let result = async {
        let pg = PgStorage::connect_for_tests(db_url(&db_name)).await?;
        apply_current_migrations(&pg).await?;
        let error = sqlx::query("SELECT 'claimed'::proxima_core.publication_record_state")
            .execute(pg.pool_for_tests())
            .await
            .expect_err("record state must have no claimed variant");
        assert_eq!(
            error
                .as_database_error()
                .and_then(sqlx::error::DatabaseError::code)
                .as_deref(),
            Some("22P02")
        );
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}
