//! The fresh CREATE set of the core migration. Requires local PG.

use std::collections::BTreeSet;
use std::fmt::Write as _;
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
async fn erased_pin_target_direct_insert_is_rejected_and_delete_records_kind() {
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
        let t = Uuid::now_v7();
        sqlx::query("INSERT INTO proxima_core.owners (owner_id, kind) VALUES ($1, 'personal')")
            .bind(owner)
            .execute(pool)
            .await?;
        let null_cooled_t = Uuid::now_v7();
        let err = sqlx::query(
            "INSERT INTO proxima_core.cooled
                 (t, handle, owner_id, kind, object_key)
             VALUES ($1, $2, $3, 'fact', 'cold/unsealed-null')",
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
                 (handle, t, kind, owner_id, schema_id)
             VALUES ($1, $2, 'fact', $3, 'core/test-v1')",
        )
        .bind(handle)
        .bind(t)
        .bind(owner)
        .execute(pool)
        .await?;

        let err = sqlx::query(
            "INSERT INTO proxima_core.cooled
                 (t, handle, owner_id, kind, object_key, origins, refs)
             SELECT t, handle, owner_id, kind, 'cold/partial-goal-refs', origins, refs
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
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
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
            "INSERT INTO proxima_core.memory (handle, t, kind, owner_id, schema_id)
             VALUES ($1, $2, 'fact', $3, 'core/test-v1')",
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
                 (handle, t, owner_id, title, state, request_id)
             VALUES ($1, $2, $3, 'reused', 'Active', 'reused-witness')",
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
            "INSERT INTO proxima_core.memory (handle, t, kind, owner_id, schema_id)
             VALUES ($1, $2, 'fact', $3, 'core/test-v1')",
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

#[tokio::test]
async fn cooled_identity_seal_freezes_all_but_transfer_remaps() {
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
                 (handle, t, kind, owner_id, schema_id)
             VALUES ($1, $2, 'fact', $3, 'core/test-v1')",
        )
        .bind(handle)
        .bind(t)
        .bind(owner)
        .execute(pool)
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.cooled
                 (t, handle, owner_id, kind, object_key, origins, refs, goal_refs)
             SELECT t, handle, owner_id, kind, 'cold/sealed', origins, refs, goal_refs
               FROM proxima_core.memory WHERE t = $1",
        )
        .bind(t)
        .execute(pool)
        .await?;

        sqlx::query("UPDATE proxima_core.cooled SET owner_id = $2 WHERE t = $1")
            .bind(t)
            .bind(destination)
            .execute(pool)
            .await?;
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
            "DROP TRIGGER goal_replay_declaration_append_only
               ON proxima_core.goal_replay_declaration",
        )
        .execute(pg.pool_for_tests())
        .await?;
        let err = ensure_core_schema_markers(pg.pool_for_tests())
            .await
            .expect_err("missing Goal replay append-only trigger must reject --stamp");
        assert!(
            err.to_string().contains("append-only trigger"),
            "marker error must name the missing Goal replay trigger: {err}"
        );
        sqlx::query(
            "CREATE TRIGGER goal_replay_declaration_append_only
             BEFORE UPDATE ON proxima_core.goal_replay_declaration
             FOR EACH ROW
             EXECUTE FUNCTION proxima_core.enforce_row_append_only()",
        )
        .execute(pg.pool_for_tests())
        .await?;

        sqlx::query(
            "ALTER TABLE proxima_core.goal_replay_declaration
             DROP CONSTRAINT goal_replay_declaration_object_chk",
        )
        .execute(pg.pool_for_tests())
        .await?;
        let err = ensure_core_schema_markers(pg.pool_for_tests())
            .await
            .expect_err("missing Goal replay declaration check must reject --stamp");
        assert!(
            err.to_string().contains("object check"),
            "marker error must name the missing Goal replay check: {err}"
        );
        sqlx::query(
            "ALTER TABLE proxima_core.goal_replay_declaration
             ADD CONSTRAINT goal_replay_declaration_object_chk
             CHECK (jsonb_typeof(declaration) = 'object')",
        )
        .execute(pg.pool_for_tests())
        .await?;

        sqlx::query(
            "ALTER TABLE proxima_core.goal_replay_declaration
             DROP CONSTRAINT goal_replay_declaration_goal_t_fkey",
        )
        .execute(pg.pool_for_tests())
        .await?;
        let err = ensure_core_schema_markers(pg.pool_for_tests())
            .await
            .expect_err("missing Goal replay Goal foreign key must reject --stamp");
        assert!(
            err.to_string().contains("goal_t foreign key"),
            "marker error must name the missing Goal replay foreign key: {err}"
        );
        sqlx::query(
            "ALTER TABLE proxima_core.goal_replay_declaration
             ADD CONSTRAINT goal_replay_declaration_goal_t_fkey
             FOREIGN KEY (goal_t) REFERENCES proxima_core.goal(t)
             ON DELETE CASCADE",
        )
        .execute(pg.pool_for_tests())
        .await?;
        ensure_core_schema_markers(pg.pool_for_tests()).await?;

        sqlx::query(
            "ALTER TABLE proxima_core.erased_pin_target
             ADD COLUMN marker_probe text",
        )
        .execute(pg.pool_for_tests())
        .await?;
        let err = ensure_core_schema_markers(pg.pool_for_tests())
            .await
            .expect_err("witness marker must reject an extra column");
        assert!(
            err.to_string().contains("exactly columns")
                || err.to_string().contains("extra columns"),
            "marker error must name witness shape drift: {err}"
        );
        sqlx::query("ALTER TABLE proxima_core.erased_pin_target DROP COLUMN marker_probe")
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

/// Every lifecycle trigger is part of the boot claim, not merely an
/// implementation detail.  Disabling one at a time must make the marker
/// probe refuse to stamp the database, and restoring it must make the probe
/// green again.
#[tokio::test]
async fn schema_markers_reject_every_reference_integrity_trigger_when_disabled() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let pool = pg.pool_for_tests();
        let triggers = [
            ("memory", "memory_pin_checks"),
            ("erased_pin_target", "erased_pin_target_insert_guard"),
            ("erased_pin_target", "erased_pin_target_append_only"),
            ("cooled", "cooled_forget_grounding"),
            ("cooled", "cooled_identity_seal"),
            ("cooled", "cooled_append_only"),
            ("goal", "goal_pin_target_checks"),
            ("wake_config", "wake_pin_target_checks"),
            ("memory", "memory_erased_pin_target"),
            ("cooled", "cooled_erased_pin_target"),
            ("goal", "goal_erased_pin_target"),
        ];
        for (table, trigger) in triggers {
            // SQL-POLICY: fixed-fragment — both interpolations come only from
            // the closed literal trigger tuple matrix immediately above.
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "ALTER TABLE proxima_core.{table} DISABLE TRIGGER {trigger}"
            )))
            .execute(pool)
            .await?;
            let err = ensure_core_schema_markers(pool)
                .await
                .expect_err("a disabled reference-integrity trigger must fail the marker probe");
            assert!(
                err.to_string().contains(table)
                    || err.to_string().contains("trigger")
                    || err.to_string().contains("marker"),
                "marker error for {table}.{trigger} should identify the trigger: {err}"
            );
            // SQL-POLICY: fixed-fragment — the same closed literal tuple
            // matrix supplies the identifiers; no external input is used.
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "ALTER TABLE proxima_core.{table} ENABLE TRIGGER {trigger}"
            )))
            .execute(pool)
            .await?;
            ensure_core_schema_markers(pool).await?;
        }
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("reference-integrity trigger marker test failed");
}

/// Function names alone are insufficient boot markers: a replaced function
/// can retain its signature while removing the lock, conflict, writer-marker,
/// or retry branch that makes the schema safe. Capture the shipped definitions,
/// corrupt each body in turn, and restore the exact text before the next probe.
#[tokio::test]
async fn schema_markers_reject_damaged_reference_integrity_function_bodies() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let pool = pg.pool_for_tests();
        let record_definition: String = sqlx::query_scalar(
            "SELECT pg_get_functiondef(
                 to_regprocedure(
                     'proxima_core.record_erased_pin_target(uuid,proxima_core.pin_target_kind)'
                 )
             )",
        )
        .fetch_one(pool)
        .await?;
        let grounding_definition: String = sqlx::query_scalar(
            "SELECT pg_get_functiondef(
                 to_regprocedure('proxima_core.cooled_forget_grounding()')
             )",
        )
        .fetch_one(pool)
        .await?;

        sqlx::raw_sql(sqlx::AssertSqlSafe(
            "CREATE OR REPLACE FUNCTION proxima_core.record_erased_pin_target(
                 target uuid, target_kind proxima_core.pin_target_kind
             ) RETURNS void LANGUAGE plpgsql AS $$
             BEGIN RETURN; END;
             $$;"
            .to_owned(),
        ))
        .execute(pool)
        .await?;
        let err = ensure_core_schema_markers(pool)
            .await
            .expect_err("a signature-compatible witness writer body must fail the marker probe");
        assert!(
            err.to_string().contains("record_erased_pin_target body"),
            "witness writer body marker should be named: {err}"
        );
        // SQL-POLICY: fixed-fragment — this is the exact definition captured
        // from the database immediately before the closed mutation above.
        sqlx::raw_sql(sqlx::AssertSqlSafe(record_definition.clone()))
            .execute(pool)
            .await?;
        ensure_core_schema_markers(pool).await?;

        sqlx::raw_sql(sqlx::AssertSqlSafe(
            "CREATE OR REPLACE FUNCTION proxima_core.cooled_forget_grounding()
             RETURNS trigger LANGUAGE plpgsql AS $$
             BEGIN RETURN NEW; END;
             $$;"
            .to_owned(),
        ))
        .execute(pool)
        .await?;
        let err = ensure_core_schema_markers(pool)
            .await
            .expect_err("a signature-compatible grounding body must fail the marker probe");
        assert!(
            err.to_string().contains("cooled_forget_grounding body"),
            "grounding body marker should be named: {err}"
        );

        // Keep all required tokens but put them in the wrong order. This
        // proves the marker checks the lock-before-growth-before-row-lock law,
        // not just the presence of a familiar error phrase.
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            "CREATE OR REPLACE FUNCTION proxima_core.cooled_forget_grounding()
             RETURNS trigger LANGUAGE plpgsql AS $$
             BEGIN
                 -- for update
                 -- using errcode = '40001'
                 -- footprint grew
                 -- not (m.t = any (dependent_ids))
                 -- lock_pin_targets
                 RETURN NEW;
             END;
             $$;"
            .to_owned(),
        ))
        .execute(pool)
        .await?;
        let err = ensure_core_schema_markers(pool)
            .await
            .expect_err("a reordered grounding body must fail the marker probe");
        assert!(
            err.to_string().contains("cooled_forget_grounding body"),
            "grounding order marker should be named: {err}"
        );
        // SQL-POLICY: fixed-fragment — this is the exact definition captured
        // from the database immediately before the closed mutation above.
        sqlx::raw_sql(sqlx::AssertSqlSafe(grounding_definition))
            .execute(pool)
            .await?;
        ensure_core_schema_markers(pool).await?;
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("reference-integrity function body marker test failed");
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

/// The presence-lane migration text IS the generator's output.
///
/// The sibling of `generated_declaration_triggers_are_the_migration_text`,
/// one lane later. Split from it rather than folded into it because
/// `0002_v009_declaration_triggers.sql` is applied in live databases: a
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

    let migration = include_str!("../migrations/0009_declared_sidecar_presence.sql");
    let baseline = include_str!("../migrations/0001_v008.sql");
    let declaration_lane = include_str!("../migrations/0002_v009_declaration_triggers.sql");
    for function in [PRESENCE_TRIGGER_FUNCTION, DELETE_GUARD_FUNCTION] {
        assert!(
            migration.contains(function),
            "0009_declared_sidecar_presence.sql does not carry a shared function \
             verbatim:\n{function}"
        );
    }
    for name in [
        "assert_declared_sidecar_present",
        "assert_row_not_still_declared",
    ] {
        assert!(
            !baseline.contains(name) && !declaration_lane.contains(name),
            "the v008 baseline and the declaration lane are applied — stamp ⊆ rows ships as \
             the additive 0009, never as an edit to a version live databases already carry"
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
        (5, 5),
        "flavor #0 registers six memory sidecars, one of them owner-pinned; only the other \
         five get a presence trigger and an orphan guard"
    );
    assert_eq!(
        repoint, 6,
        "every registered memory sidecar gets the UPDATE-of-key guard, owner-pinned included"
    );
    assert_eq!(
        artifacts.len(),
        presence + orphan + repoint,
        "every generated artifact belongs to one of the three families"
    );
    for artifact in &artifacts {
        assert!(
            migration.contains(&artifact.forward),
            "0009_declared_sidecar_presence.sql does not carry the generator's output \
             verbatim:\n{}",
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

/// The reference-integrity lane is deliberately one additive migration. Its
/// trigger body is pinned here so a future rewrite cannot silently collapse
/// the origin-only and reference-capable target sets back into one check.
#[test]
fn reference_integrity_migration_is_set_based_and_keeps_baselines_frozen() {
    let migration = include_str!("../migrations/0003_v010_reference_integrity.sql");
    let goal_refs_lane = include_str!("../migrations/0004_v011_goal_refs.sql");
    let erased_targets_lane = include_str!("../migrations/0005_erased_pin_targets.sql");
    let baseline = include_str!("../migrations/0001_v008.sql");
    let declaration_lane = include_str!("../migrations/0002_v009_declaration_triggers.sql");
    assert!(
        !baseline.contains("cooled_origins_no_null_chk")
            && !declaration_lane.contains("cooled_origins_no_null_chk"),
        "the new cooled witness belongs to the additive 0003 migration"
    );
    assert!(
        !migration.contains("erased_pin_target") && !goal_refs_lane.contains("erased_pin_target"),
        "0003 and 0004 are frozen; erased-target behavior belongs in 0005"
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
            "0003_v010_reference_integrity.sql is missing {fragment:?}"
        );
    }
    for fragment in [
        "CREATE TYPE proxima_core.pin_target_kind",
        "CREATE TABLE proxima_core.erased_pin_target",
        "CREATE OR REPLACE FUNCTION proxima_core.lock_pin_targets",
        "CREATE TRIGGER memory_erased_pin_target",
        "CREATE TRIGGER cooled_erased_pin_target",
        "CREATE TRIGGER goal_erased_pin_target",
        "CREATE TRIGGER cooled_identity_seal",
        "CREATE TRIGGER cooled_append_only",
        "NEW.goal_refs",
        "historical_restore",
        "e.kind = 'goal'",
    ] {
        assert!(
            erased_targets_lane.contains(fragment),
            "0005_erased_pin_targets.sql is missing {fragment:?}"
        );
    }
}

/// Exact Goal replay is new persisted state, so it gets one additive lane;
/// no already-shipped file may absorb it without invalidating `SQLx` checksums.
#[test]
fn goal_replay_declaration_is_an_additive_immutable_lane() {
    let migration = include_str!("../migrations/0006_v013_goal_replay_declaration.sql");
    for frozen in [
        include_str!("../migrations/0001_v008.sql"),
        include_str!("../migrations/0002_v009_declaration_triggers.sql"),
        include_str!("../migrations/0003_v010_reference_integrity.sql"),
        include_str!("../migrations/0004_v011_goal_refs.sql"),
        include_str!("../migrations/0005_erased_pin_targets.sql"),
    ] {
        assert!(
            !frozen.contains("goal_replay_declaration"),
            "Goal replay declarations belong only in the additive v0.0.13 lane"
        );
    }
    for fragment in [
        "CREATE TABLE proxima_core.goal_replay_declaration",
        "goal_t uuid PRIMARY KEY",
        "REFERENCES proxima_core.goal (t) ON DELETE CASCADE",
        "goal_replay_declaration_object_chk",
        "goal_replay_edge_count_chk",
        "CREATE TRIGGER goal_replay_declaration_append_only",
        "EXECUTE FUNCTION proxima_core.enforce_row_append_only()",
    ] {
        assert!(
            migration.contains(fragment),
            "0006_v013_goal_replay_declaration.sql is missing {fragment:?}"
        );
    }
}

/// Staged upload identity is new persisted state and therefore lives only in
/// the additive 0007 lane; no frozen migration may absorb the column.
#[test]
fn upload_content_identity_is_an_additive_immutable_lane() {
    let migration = include_str!("../migrations/0007_upload_content_identity.sql");
    for frozen in [
        include_str!("../migrations/0001_v008.sql"),
        include_str!("../migrations/0002_v009_declaration_triggers.sql"),
        include_str!("../migrations/0003_v010_reference_integrity.sql"),
        include_str!("../migrations/0004_v011_goal_refs.sql"),
        include_str!("../migrations/0005_erased_pin_targets.sql"),
        include_str!("../migrations/0006_v013_goal_replay_declaration.sql"),
    ] {
        assert!(
            !frozen.contains("blob_uploads_content_hash_chk"),
            "upload content identity belongs only in the additive 0007 lane"
        );
    }
    for fragment in [
        "ADD COLUMN content_hash bytea",
        "blob_uploads_content_hash_chk",
        "octet_length(content_hash) = 32",
        "SET content_hash = b.content_hash",
        "WHERE u.blob_id = b.blob_id",
        "u.owner_id = b.owner_id",
        "u.status = 'completed'",
        "u.completed_at IS NOT NULL",
        "octet_length(u.sha256) = 32",
        "COALESCE(u.mounted_from_upload_id, u.upload_id)::text",
        "blob_uploads_terminal_content_idx",
    ] {
        assert!(
            migration.contains(fragment),
            "0007_upload_content_identity.sql is missing {fragment:?}"
        );
    }
}

/// `SQLx` records SHA-384 checksums. Pin the two already-landed files here so
/// moving their behavior into a new additive lane cannot silently rewrite the
/// bytes that a live database may already have recorded.
#[test]
fn frozen_reference_integrity_migration_checksums_are_unchanged() {
    fn hex(bytes: &[u8]) -> String {
        let mut text = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            write!(&mut text, "{byte:02x}").expect("writing to String cannot fail");
        }
        text
    }

    let migrator = proxima_storage_pg::core_migrator();
    let v3 = migrator
        .iter()
        .find(|migration| migration.version == 3)
        .expect("version 3 is the frozen reference-integrity migration");
    let v4 = migrator
        .iter()
        .find(|migration| migration.version == 4)
        .expect("version 4 is the frozen Goal-reference migration");
    assert_eq!(
        hex(v3.checksum.as_ref()),
        "12f6791f63499f45a6af1233b3702a838af7ee8c06b23564a683bf3a6a363f743cc10c922427e3dfc1c2c1c37fd7fab2"
    );
    assert_eq!(
        hex(v4.checksum.as_ref()),
        "625f7bf3e2fff4064df91be7be755b9455914f8366bcaa5b80b32afa2d802cdc487ba558851074d6602a1877494db119"
    );
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
                     (handle, t, kind, owner_id, schema_id, sidecar_tables)
                 VALUES ($1, $1, 'fact', $2, 'core/agent-note-v1', $3)",
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

/// The same guardrail, over the two families 0009 added.
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
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
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

/// The `row ⊆ stamp` direction was INSERT-only until 0009: a legal row could
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
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
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
                     (handle, t, kind, owner_id, schema_id, sidecar_tables)
                 VALUES ($1, $1, 'fact', $2, 'core/agent-note-v1', $3)",
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
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
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

        let scoped = PgStorage::connect(&url).await?;
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
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
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

/// A v0.0.8 database upgrades through the current head in place — no reset,
/// no data loss.
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
#[allow(clippy::too_many_lines)]
async fn a_v008_database_upgrades_to_head_in_place() {
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
            vec![1, 2, 3, 4, 5, 6, 7, 8, 9],
            "the upgrade appends every migration after the baseline; it does not re-apply or replace the \
             baseline"
        );

        let memory_pin_checks: String = sqlx::query_scalar(
            "SELECT pg_get_functiondef(
                 'proxima_core.memory_pin_checks()'::regprocedure
             )",
        )
        .fetch_one(pool)
        .await?;
        assert!(
            memory_pin_checks.contains("historical_restore")
                && memory_pin_checks.contains("NEW.goal_refs"),
            "the final migration must compose erased-target restore with typed Goal refs"
        );
        let cooled_seal: String = sqlx::query_scalar(
            "SELECT pg_get_functiondef(
                 'proxima_core.cooled_identity_seal()'::regprocedure
             )",
        )
        .fetch_one(pool)
        .await?;
        assert!(
            cooled_seal.contains("m.goal_refs") && cooled_seal.contains("NEW.goal_refs"),
            "the final cooled seal must cover the split Goal-reference column"
        );
        let cooled_append_only: String = sqlx::query_scalar(
            "SELECT pg_get_functiondef(
                 'proxima_core.cooled_append_only()'::regprocedure
             )",
        )
        .fetch_one(pool)
        .await?;
        assert!(
            cooled_append_only.contains("NEW.goal_refs")
                && cooled_append_only.contains("OLD.goal_refs")
                && cooled_append_only.contains("NEW.cold_digest")
                && cooled_append_only.contains("OLD.cold_digest"),
            "the final cooled append-only guard must freeze the split Goal-reference column"
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
    result.expect("v0.0.8 -> head in-place upgrade failed");
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

struct UploadContentIdentityFixture {
    completed_upload: Uuid,
    pending_upload: Uuid,
    terminal_upload: Uuid,
    cross_owner_upload: Uuid,
    noncanonical_upload: Uuid,
    hash: Vec<u8>,
}

async fn seed_upload_content_identity_fixture(
    pool: &sqlx::PgPool,
) -> Result<UploadContentIdentityFixture, sqlx::Error> {
    let owner = Uuid::now_v7();
    let blob_id = Uuid::now_v7();
    let completed_upload = Uuid::now_v7();
    let pending_upload = Uuid::now_v7();
    let terminal_upload = Uuid::now_v7();
    let cross_owner_upload = Uuid::now_v7();
    let noncanonical_upload = Uuid::now_v7();
    let other_owner = Uuid::now_v7();
    let hash = vec![91_u8; 32];
    sqlx::query(
        "INSERT INTO proxima_core.owners (owner_id, kind)
         VALUES ($1, 'personal'), ($2, 'personal')",
    )
    .bind(owner)
    .bind(other_owner)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.blob (blob_id, owner_id, schema_id, content_hash)
         VALUES ($1, $2, 'core/uploaded-blob-v1', $3)",
    )
    .bind(blob_id)
    .bind(owner)
    .bind(&hash)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.blob_uploads
            (upload_id, owner_id, bucket, object_key, filename, mime,
             expected_byte_len, status, blob_id, sha256, expires_at, completed_at)
         VALUES
            ($1, $3, 'bucket', 'objects/' || $1::text, 'a', 'application/octet-stream',
             1, 'completed', $4, $5, now() + interval '1 day', now()),
            ($2, $3, 'bucket', 'pending/pending', 'b', 'application/octet-stream',
             1, 'pending', NULL, NULL, now() + interval '1 day', NULL)",
    )
    .bind(completed_upload)
    .bind(pending_upload)
    .bind(owner)
    .bind(blob_id)
    .bind(&hash)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.blob_uploads
            (upload_id, owner_id, bucket, object_key, filename, mime,
             expected_byte_len, status, blob_id, sha256, expires_at, completed_at)
         VALUES
            ($1, $2, 'bucket', 'objects/not-the-minting-id', 'e',
             'application/octet-stream', 1, 'completed', $3, $4,
             now() + interval '1 day', now())",
    )
    .bind(noncanonical_upload)
    .bind(owner)
    .bind(blob_id)
    .bind(&hash)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.blob_uploads
            (upload_id, owner_id, bucket, object_key, filename, mime,
             expected_byte_len, status, blob_id, sha256, expires_at, completed_at)
         VALUES
            ($1, $3, 'bucket', 'objects/' || $1::text, 'c',
             'application/octet-stream', 1, 'aborted', $4, $6,
             now() + interval '1 day', NULL),
            ($2, $5, 'bucket', 'objects/' || $2::text, 'd',
             'application/octet-stream', 1, 'completed', $4, $6,
             now() + interval '1 day', now())",
    )
    .bind(terminal_upload)
    .bind(cross_owner_upload)
    .bind(owner)
    .bind(blob_id)
    .bind(other_owner)
    .bind(&hash)
    .execute(pool)
    .await?;
    Ok(UploadContentIdentityFixture {
        completed_upload,
        pending_upload,
        terminal_upload,
        cross_owner_upload,
        noncanonical_upload,
        hash,
    })
}

/// Migration 0007 learns exact staged content identity without guessing
/// interrupted uploads. Rows already carrying a complete, canonical publication inherit
/// its address; every incomplete or malformed row remains unknown.
#[tokio::test]
async fn upload_content_identity_backfills_only_rows_with_an_exact_blob() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pool = sqlx::PgPool::connect(&db_url(&db_name)).await?;
        apply_core_migrations(&pool, 1..=6).await?;
        let UploadContentIdentityFixture {
            completed_upload,
            pending_upload,
            terminal_upload,
            cross_owner_upload,
            noncanonical_upload,
            hash,
        } = seed_upload_content_identity_fixture(&pool).await?;

        apply_core_migrations(&pool, 7..=7).await?;
        let completed_hash: Option<Vec<u8>> = sqlx::query_scalar(
            "SELECT content_hash FROM proxima_core.blob_uploads WHERE upload_id = $1",
        )
        .bind(completed_upload)
        .fetch_one(&pool)
        .await?;
        let pending_hash: Option<Vec<u8>> = sqlx::query_scalar(
            "SELECT content_hash FROM proxima_core.blob_uploads WHERE upload_id = $1",
        )
        .bind(pending_upload)
        .fetch_one(&pool)
        .await?;
        let terminal_hash: Option<Vec<u8>> = sqlx::query_scalar(
            "SELECT content_hash FROM proxima_core.blob_uploads WHERE upload_id = $1",
        )
        .bind(terminal_upload)
        .fetch_one(&pool)
        .await?;
        let cross_owner_hash: Option<Vec<u8>> = sqlx::query_scalar(
            "SELECT content_hash FROM proxima_core.blob_uploads WHERE upload_id = $1",
        )
        .bind(cross_owner_upload)
        .fetch_one(&pool)
        .await?;
        let noncanonical_hash: Option<Vec<u8>> = sqlx::query_scalar(
            "SELECT content_hash FROM proxima_core.blob_uploads WHERE upload_id = $1",
        )
        .bind(noncanonical_upload)
        .fetch_one(&pool)
        .await?;
        assert_eq!(completed_hash, Some(hash));
        assert_eq!(pending_hash, None, "an interrupted upload is not guessed");
        assert_eq!(
            terminal_hash, None,
            "terminal cleanup is not promoted back into publication authority"
        );
        assert_eq!(
            cross_owner_hash, None,
            "a legacy cross-owner pointer is not blessed as content authority"
        );
        assert_eq!(
            noncanonical_hash, None,
            "a completed row with an unminted locator is not blessed"
        );

        let malformed = sqlx::query(
            "UPDATE proxima_core.blob_uploads SET content_hash = decode('00', 'hex')
              WHERE upload_id = $1",
        )
        .bind(pending_upload)
        .execute(&pool)
        .await
        .expect_err("content identity is exactly 32 bytes");
        assert_eq!(
            malformed
                .as_database_error()
                .and_then(sqlx::error::DatabaseError::code),
            Some("23514".into())
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("upload content identity migration failed");
}

/// Migration 0004 splits Goal references out of `refs` into their own column. The
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
        // Stop one short of the split: this is the 0003 schema, where a
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

        // A database paused at the frozen 0004 split must reach the same
        // final catalog as a fresh apply when the witness lane is replayed.
        apply_core_migrations(&pool, 5..=5).await?;
        assert!(
            sqlx::query_scalar::<_, bool>(
                "SELECT to_regclass('proxima_core.erased_pin_target') IS NOT NULL",
            )
            .fetch_one(&pool)
            .await?,
            "the 0005 witness lane must apply after the frozen 0004 split"
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
