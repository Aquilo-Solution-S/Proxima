//! The fresh CREATE set of the core migration. Requires local PG.

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
            "cold_purge_pending",
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
            "announce_owner_seq_idx",
            "idx_embeddings_vec_hnsw",
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
        // Derived from the migrator rather than a hardcoded count: v0.0.8 was
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

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn reference_integrity_migration_enforces_goal_refs_and_cooled_arrays() {
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
                 (handle, t, owner_id, title, state, request_id)
             VALUES ($1, $2, $3, 'reference target', 'Active', 'reference-goal')",
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
                 (handle, t, kind, owner_id, schema_id, goal_refs)
             VALUES ($1, $2, 'fact', $3, 'test/fact-v1', ARRAY[$4]::uuid[])",
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
                 (handle, t, kind, owner_id, schema_id)
             VALUES ($1, $2, 'fact', $3, 'test/source-v1')",
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
                 (handle, t, kind, owner_id, schema_id, origins, content_id)
             VALUES ($1, $2, 'abstraction', $3, 'test/abstraction-v1',
                     ARRAY[$4, $5]::uuid[], $6)",
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
                 (t, handle, owner_id, kind, object_key, origins)
             VALUES ($1, $2, $3, 'fact', 'cold/null-origin', ARRAY[NULL]::uuid[])",
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
                 (t, handle, owner_id, kind, object_key, refs)
             VALUES ($1, $2, $3, 'fact', 'cold/null-ref', ARRAY[NULL]::uuid[])",
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

        // The lexical stamp lives on the projection, beside the vector.
        sqlx::query(
            "ALTER TABLE proxima_core.projection
             DROP CONSTRAINT projection_lexical_language_fkey",
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
            "ALTER TABLE proxima_core.projection
             ADD CONSTRAINT projection_lexical_language_fkey
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

/// `proxima_core.flavor_surface` is the registry as the database sees it,
/// and `memory.sidecar_tables` is constrained to be a subset of it.
///
/// Two things are asserted, because the table is only worth having if both
/// hold: the seeded rows are exactly flavor #0's declared sidecar surfaces,
/// and a stamp naming an undeclared table is refused at write time rather
/// than accepted and then walked past by every registry-driven sweep.
#[tokio::test]
async fn flavor_surface_is_the_registry_and_the_stamp_must_be_a_subset() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());

    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }

    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;

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
                 (handle, t, kind, owner_id, schema_id, sidecar_tables)
             VALUES ($1, $1, 'fact', $2, 'core/agent-note-v1',
                     ARRAY['proxima_core.not_a_declared_surface'])",
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
    let baseline = include_str!("../migrations/0001_v008.sql");
    for statement in core.forward() {
        assert!(
            baseline.contains(statement),
            "0001_v008.sql does not carry the generator's output verbatim:\n{statement}"
        );
    }

    // The code flavor's baseline is checked from its own crate's test
    // (`flavors/code/tests/migrations_pg.rs`), which is where that file is.
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

/// The migration text IS the declaration-integrity generator's output.
///
/// The same pin as `generator_output_is_the_migration_text`, one layer
/// down: `crates/storage-pg/src/integrity.rs` has the same two consumers —
/// the migration author, who pastes its output into the migration, and
/// `ensure_declaration_triggers`, which re-runs it against the catalog at
/// boot. A hand edit to the trigger block, or a generator change the
/// migration did not follow, fails here rather than at the next boot.
///
/// The text lives in `0002_v009_declaration_triggers.sql`, not in the v008
/// baseline: `0001_v008.sql` is frozen, and pasting into it would change the
/// checksum of a version live databases have already applied. The second
/// assertion below is what keeps that decision from quietly rotting.
#[test]
fn generated_declaration_triggers_are_the_migration_text() {
    use proxima_storage_pg::integrity::DECLARATION_TRIGGER_FUNCTION;

    let migration = include_str!("../migrations/0002_v009_declaration_triggers.sql");
    let baseline = include_str!("../migrations/0001_v008.sql");
    assert!(
        migration.contains(DECLARATION_TRIGGER_FUNCTION),
        "0002_v009_declaration_triggers.sql does not carry the shared trigger function \
         verbatim:\n{DECLARATION_TRIGGER_FUNCTION}"
    );
    assert!(
        !baseline.contains("assert_memory_declares_sidecar"),
        "the v008 baseline is frozen — declaration integrity ships as the additive 0002, \
         never as an edit to a version live databases have already applied"
    );

    let artifacts = frozen_core_sidecars()
        .declaration_trigger_artifacts("core")
        .expect("core's declaration triggers");
    assert_eq!(
        artifacts.len(),
        6,
        "flavor #0 registers six memory sidecars; a seventh needs its trigger in the migration"
    );
    for artifact in &artifacts {
        assert!(
            migration.contains(&artifact.forward),
            "0002_v009_declaration_triggers.sql does not carry the generator's output \
             verbatim:\n{}",
            artifact.forward
        );
    }
}

/// The reference-integrity lane is deliberately one additive migration. Its
/// trigger body is pinned here so a future rewrite cannot silently collapse
/// the origin-only and reference-capable target sets back into one check.
#[test]
fn reference_integrity_migration_is_set_based_and_keeps_baselines_frozen() {
    let migration = include_str!("../migrations/0003_v010_reference_integrity.sql");
    let baseline = include_str!("../migrations/0001_v008.sql");
    let declaration_lane = include_str!("../migrations/0002_v009_declaration_triggers.sql");
    assert!(
        !baseline.contains("cooled_origins_no_null_chk")
            && !declaration_lane.contains("cooled_origins_no_null_chk"),
        "the new cooled witness belongs to the additive v0.0.10 migration"
    );
    for fragment in [
        "CREATE OR REPLACE FUNCTION proxima_core.memory_pin_checks()",
        "FROM unnest(NEW.origins)",
        "FROM unnest(NEW.refs)",
        "FROM proxima_core.goal",
        "WHERE t = ANY (NEW.origins || NEW.refs)",
        "ORDER BY t",
        "FOR SHARE",
        "cooled_origins_no_null_chk",
        "cooled_refs_no_null_chk",
    ] {
        assert!(
            migration.contains(fragment),
            "v0.0.10 reference-integrity migration is missing {fragment:?}"
        );
    }
}

/// `task_goal_v1` hangs off a Goal, so no memory ever stamps it — and a
/// trigger asking `proxima_core.memory` about it would refuse every Goal
/// sidecar write there is.
///
/// Asserted rather than assumed, because the generator's filter
/// (`memory_insert` or `memory_load_batch` present) is what excludes it and
/// nothing else would notice if that filter widened.
#[test]
fn a_goal_sidecar_gets_no_declaration_trigger() {
    let tables: Vec<String> = frozen_core_sidecars()
        .declaration_trigger_artifacts("core")
        .expect("core's declaration triggers")
        .iter()
        .map(|artifact| artifact.forward.clone())
        .collect();
    assert!(
        !tables
            .iter()
            .any(|forward| forward.contains("proxima_core.task_goal_v1")),
        "a Goal sidecar is not a memory sidecar: {tables:?}"
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
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let pool = pg.pool_for_tests();

        let owner_id = Uuid::now_v7();
        sqlx::query("INSERT INTO proxima_core.owners (owner_id, kind) VALUES ($1, 'personal')")
            .bind(owner_id)
            .execute(pool)
            .await?;

        // Two memories of the same schema. One declares the note sidecar,
        // one declares nothing — so the ONLY difference between the two
        // inserts below is the stamp.
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
                vec!["proxima_core.agent_note_v1".to_owned()]
            } else {
                Vec::new()
            };
            sqlx::query(
                "INSERT INTO proxima_core.memory
                     (handle, t, kind, owner_id, schema_id, sidecar_tables)
                 VALUES ($1, $1, 'fact', $2, 'core/agent-note-v1', $3)",
            )
            .bind(handle)
            .bind(owner_id)
            .bind(&stamp)
            .execute(pool)
            .await?;
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

        // The same statement, against the memory that declared it.
        sqlx::query(
            "INSERT INTO proxima_core.agent_note_v1 (t, note_id, title, body, tags)
             VALUES ($1, $1, 'title', 'body', '{}')",
        )
        .bind(ids[1])
        .execute(pool)
        .await
        .expect("a declared sidecar row is admitted");

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("declaration integrity trigger test failed");
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
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
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
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
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
                     (handle, t, kind, owner_id, schema_id, sidecar_tables)
                 VALUES ($1, $1, 'fact', $2, 'core/agent-note-v1', $3)",
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

/// Apply the frozen v0.0.8 baseline and nothing else, exactly the way a
/// database deployed on v0.0.8 carries it: the file's own bytes, and one
/// ledger row whose checksum is the embedded migration's.
///
/// The checksum is taken from the migrator rather than recomputed here, so
/// this fixture can never disagree with what `ensure_core_ledger_compatible`
/// compares against.
async fn seed_a_live_v008_database(pool: &sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let migrator = proxima_storage_pg::core_migrator();
    let baseline = migrator
        .iter()
        .find(|migration| migration.version == 1)
        .expect("the embedded core set carries the v008 baseline");

    // SQL-POLICY: fixed-fragment — the migration's own embedded text, the
    // same bytes `core_migrator().run()` would execute. Nothing from outside
    // the binary reaches it.
    sqlx::raw_sql(baseline.sql.clone()).execute(pool).await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS public._sqlx_migrations (
             version bigint PRIMARY KEY,
             description text NOT NULL,
             installed_on timestamptz NOT NULL DEFAULT now(),
             success boolean NOT NULL,
             checksum bytea NOT NULL,
             execution_time bigint NOT NULL
         )",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO public._sqlx_migrations
             (version, description, success, checksum, execution_time)
         VALUES ($1, $2, true, $3, 0)",
    )
    .bind(baseline.version)
    .bind(baseline.description.as_ref())
    .bind(baseline.checksum.as_ref())
    .execute(pool)
    .await?;
    Ok(())
}

/// A v0.0.8 database upgrades through v0.0.10 in place — no reset, no data loss.
///
/// This is the whole reason the declaration triggers ship as
/// `0002_v009_declaration_triggers.sql` instead of being pasted into the
/// baseline. Pasting them in would change the checksum of version 1, which
/// every deployed database has already recorded, and
/// `ensure_core_ledger_compatible` would answer `SchemaResetRequired` — a
/// forced destructive reset with no schema reason behind it.
///
/// Three things are asserted, because the upgrade is only real if all three
/// hold: boot does not demand a reset, the ledger carries every version, and
/// the installed invariants are actually live afterwards.
#[tokio::test]
async fn a_v008_database_upgrades_to_v011_in_place() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());

    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }

    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        let pool = pg.pool_for_tests();
        seed_a_live_v008_database(pool).await?;

        // A row written under v0.0.8 that the upgrade must not disturb. The
        // `BEFORE INSERT` triggers constrain what is written from here on;
        // they do not validate history, and a migration that dropped this
        // row would be exactly the destructive reset being avoided.
        let owner = Uuid::now_v7();
        let handle = Uuid::now_v7();
        let survivor = Uuid::now_v7();
        sqlx::query("INSERT INTO proxima_core.owners (owner_id, kind) VALUES ($1, 'personal')")
            .bind(owner)
            .execute(pool)
            .await?;
        sqlx::query(
            "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
             VALUES ($1, 'fact', 'upgrade.probe.v1', $2, $3)",
        )
        .bind(handle)
        .bind(owner)
        .bind(survivor)
        .execute(pool)
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.memory (handle, t, kind, owner_id, schema_id)
             VALUES ($1, $2, 'fact', $3, 'upgrade.probe.v1')",
        )
        .bind(handle)
        .bind(survivor)
        .bind(owner)
        .execute(pool)
        .await?;

        pg.run_migrations().await.map_err(|err| {
            format!("a live v0.0.8 database must upgrade in place, not reset: {err}")
        })?;

        let versions: Vec<i64> = sqlx::query_scalar(
            "SELECT version FROM public._sqlx_migrations
              WHERE success AND version <= $1 ORDER BY version",
        )
        .bind(proxima_storage_pg::CORE_MIGRATION_VERSION_CEILING)
        .fetch_all(pool)
        .await?;
        assert_eq!(
            versions,
            vec![1, 2, 3, 4],
            "the upgrade appends v0.0.9, v0.0.10 and v0.0.11; it does not re-apply or replace the \
             baseline"
        );

        let still_there: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM proxima_core.memory WHERE t = $1)")
                .bind(survivor)
                .fetch_one(pool)
                .await?;
        assert!(still_there, "the upgrade must not touch v0.0.8 data");

        // The boot guardrail is the same one `Engine::boot` runs, so this
        // asserts the upgraded catalog is what the generator expects — not
        // merely that a migration ran.
        proxima_storage_pg::integrity::ensure_declaration_triggers(pool, &frozen_core_sidecars())
            .await
            .map_err(|err| {
                format!("the upgraded database must satisfy the boot guardrail: {err}")
            })?;

        proxima_storage_pg::ensure_core_schema_current(pool).await?;

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("v0.0.8 -> v0.0.10 in-place upgrade failed");
}

/// Applies the embedded core migrations in `versions`, straight from the
/// shipped files, so a test can stand a database up at an older schema and
/// then step it forward one migration at a time. The ledger is deliberately
/// not written: nothing here goes on to call `run_migrations`.
async fn apply_core_migrations(
    pool: &sqlx::PgPool,
    versions: std::ops::RangeInclusive<i64>,
) -> Result<(), sqlx::Error> {
    let migrator = proxima_storage_pg::core_migrator();
    for migration in migrator.iter().filter(|m| versions.contains(&m.version)) {
        sqlx::raw_sql(migration.sql.clone()).execute(pool).await?;
    }
    Ok(())
}

/// v0.0.11 splits Goal references out of `refs` into their own column. The
/// rows already on disk were written when `refs` was the only place a Goal
/// reference could go, so the migration has to move them -- and move only
/// them, leaving Memory references where they are.
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn goal_refs_migration_backfills_goals_out_of_the_legacy_refs_column() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pool = sqlx::PgPool::connect(&url).await?;
        // Stop one short of the split: this is the v0.0.10 schema, where a
        // Goal t in `refs` is exactly what the writer produced.
        apply_core_migrations(&pool, 1..=3).await?;
        assert!(
            !sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS (
                     SELECT 1 FROM information_schema.columns
                      WHERE table_schema = 'proxima_core'
                        AND table_name = 'memory'
                        AND column_name = 'goal_refs'
                 )",
            )
            .fetch_one(&pool)
            .await?,
            "the fixture must be seeded before the column exists"
        );

        let owner = Uuid::now_v7();
        sqlx::query("INSERT INTO proxima_core.owners (owner_id, kind) VALUES ($1, 'personal')")
            .bind(owner)
            .execute(&pool)
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
        .execute(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.goal
                 (handle, t, owner_id, title, state, request_id)
             VALUES ($1, $2, $3, 'legacy target', 'Active', 'legacy-goal')",
        )
        .bind(goal_handle)
        .bind(goal_t)
        .bind(owner)
        .execute(&pool)
        .await?;

        let seed_fact = async |refs: Vec<Uuid>| -> Result<Uuid, sqlx::Error> {
            let handle = Uuid::now_v7();
            let t = Uuid::now_v7();
            sqlx::query(
                "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
                 VALUES ($1, 'fact', 'test/fact-v1', $2, $3)",
            )
            .bind(handle)
            .bind(owner)
            .bind(t)
            .execute(&pool)
            .await?;
            sqlx::query(
                "INSERT INTO proxima_core.memory
                     (handle, t, kind, owner_id, schema_id, refs)
                 VALUES ($1, $2, 'fact', $3, 'test/fact-v1', $4)",
            )
            .bind(handle)
            .bind(t)
            .bind(owner)
            .bind(&refs)
            .execute(&pool)
            .await?;
            Ok(t)
        };

        let plain = seed_fact(Vec::new()).await?;
        let memory_only = seed_fact(vec![plain]).await?;
        // The row the migration exists for: one Goal and one Memory in the
        // same untyped array.
        let mixed = seed_fact(vec![plain, goal_t]).await?;
        let goal_only = seed_fact(vec![goal_t]).await?;

        apply_core_migrations(&pool, 4..=4).await?;

        let stored = async |t: Uuid| -> Result<(Vec<Uuid>, Vec<Uuid>), sqlx::Error> {
            sqlx::query_as("SELECT refs, goal_refs FROM proxima_core.memory WHERE t = $1")
                .bind(t)
                .fetch_one(&pool)
                .await
        };
        assert_eq!(stored(plain).await?, (vec![], vec![]));
        assert_eq!(
            stored(memory_only).await?,
            (vec![plain], vec![]),
            "a Memory reference must stay in refs and must not be re-typed as a Goal"
        );
        assert_eq!(
            stored(mixed).await?,
            (vec![plain], vec![goal_t]),
            "the split must partition a mixed array, not move the whole of it"
        );
        assert_eq!(stored(goal_only).await?, (vec![], vec![goal_t]));

        // A backfilled row has to be findable on the spine the reader now
        // queries -- that is what makes it project Visible rather than
        // silently disappear. The Goal-target read lane scans `goal_refs`,
        // so this is the predicate that decides it.
        let mut found: Vec<Uuid> = sqlx::query_scalar(
            "SELECT t FROM proxima_core.memory WHERE goal_refs && ARRAY[$1]::uuid[]",
        )
        .bind(goal_t)
        .fetch_all(&pool)
        .await?;
        found.sort();
        let mut want = vec![mixed, goal_only];
        want.sort();
        assert_eq!(
            found, want,
            "every backfilled row must be found on the Goal spine"
        );

        // And the move is a move, not a copy: the pre-split predicate finds
        // nothing, so no reader can reach the Goal through `refs` any more.
        let stale: Vec<Uuid> =
            sqlx::query_scalar("SELECT t FROM proxima_core.memory WHERE refs && ARRAY[$1]::uuid[]")
                .bind(goal_t)
                .fetch_all(&pool)
                .await?;
        assert!(
            stale.is_empty(),
            "the backfill must move Goal ids out of refs, not copy them: {stale:?}"
        );

        // The backfill drops the append-only guard for one statement. It has
        // to be back afterwards, or the split would leave `memory` writable.
        let err = sqlx::query("UPDATE proxima_core.memory SET refs = '{}' WHERE t = $1")
            .bind(mixed)
            .execute(&pool)
            .await
            .expect_err("append-only must be restored after the backfill");
        assert!(
            err.to_string().contains("append-only"),
            "the restored guard must be the append-only one: {err}"
        );

        // And the new column joins that guard rather than becoming the one
        // mutable pin array.
        let err = sqlx::query("UPDATE proxima_core.memory SET goal_refs = '{}' WHERE t = $1")
            .bind(mixed)
            .execute(&pool)
            .await
            .expect_err("goal_refs must be append-only too");
        assert!(
            err.to_string().contains("append-only"),
            "goal_refs must be covered by the append-only guard: {err}"
        );
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("goal_refs backfill test failed");
}
