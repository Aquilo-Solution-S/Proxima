//! Apply core + flavor migrations to a fresh DB and verify the
//! current `proxima_code` schema shape.

use proxima_pg_testkit::{create_db, db_url, drop_db, unique_db_name};
use proxima_storage_pg::PgStorage;

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn flavor_migrations_apply_to_fresh_db() {
    let db_name = unique_db_name("proxima_test");
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?; // core
        proxima_code::migrator().run(pg.pool_for_tests()).await?; // flavor

        // Verify the flavor sidecar tables exist.
        for table in [
            "commit_v1",
            "file_revision_v1",
            "code_chunk_v1",
            "commit_summary_v1",
            "work_requested_v1",
            "test_requested_v1",
            "test_requested_criterion_v1",
            "acceptance_criteria_v1",
            "acceptance_criterion_v1",
            "execution_plan_v1",
            "execution_plan_item_v1",
            "execution_result_v1",
            "test_result_v1",
            "acceptance_verification_v1",
            "acceptance_summary_v1",
            // Call sites live on the chunk payload; work assignment is a
            // node.
            "code_chunk_call_v1",
            "work_assignment_v1",
        ] {
            let row = sqlx::query(
                "SELECT 1 AS ok FROM information_schema.tables
                 WHERE table_schema = 'proxima_code' AND table_name = $1",
            )
            .bind(table)
            .fetch_optional(pg.pool_for_tests())
            .await?;
            assert!(row.is_some(), "expected table proxima_code.{table}");
        }
        // The workspace-runner subsystem is gone (drop_workspace_mode /
        // drop_workspace_review); its tables must not survive a fresh apply.
        for dropped in [
            "workspace_run_v1",
            "workspace_decision_v1",
            "workspace_review_v1",
            // The typed edge sidecar. A flavor cannot declare one any more,
            // and this one could never hold a second call site anyway.
            "code_calls_v1",
        ] {
            let row = sqlx::query(
                "SELECT 1 AS ok FROM information_schema.tables
                 WHERE table_schema = 'proxima_code' AND table_name = $1",
            )
            .bind(dropped)
            .fetch_optional(pg.pool_for_tests())
            .await?;
            assert!(row.is_none(), "proxima_code.{dropped} should be dropped");
        }

        // Verify the M5 core tables exist.
        for table in ["memory", "memory_head", "announce"] {
            let row = sqlx::query(
                "SELECT 1 AS ok FROM information_schema.tables
                 WHERE table_schema = 'proxima_core' AND table_name = $1",
            )
            .bind(table)
            .fetch_optional(pg.pool_for_tests())
            .await?;
            assert!(row.is_some(), "expected table proxima_core.{table}");
        }

        for (table, column) in [
            ("repos", "owner_kind"),
            ("repo_ingestion_runs", "owner_kind"),
            ("repo_ingestion_runs", "status"),
            ("repo_ingestion_runs", "stage"),
            ("file_revision_v1", "state"),
            ("code_chunk_v1", "state"),
            ("execution_plan_item_v1", "kind"),
            ("execution_result_v1", "status"),
            ("test_result_v1", "status"),
            ("acceptance_verification_v1", "status"),
        ] {
            assert_enum_column(pg.pool_for_tests(), "proxima_code", table, column).await?;
        }

        assert_owner_ref_constraints(pg.pool_for_tests()).await?;

        for table in [
            "code_chunk_v1",
            "file_revision_v1",
            "commit_v1",
            "commit_summary_v1",
        ] {
            let present: bool = sqlx::query_scalar(
                "SELECT EXISTS (
                     SELECT 1 FROM information_schema.columns
                      WHERE table_schema = 'proxima_code'
                        AND table_name = $1
                        AND column_name = 'embed_text'
                 )",
            )
            .bind(table)
            .fetch_one(pg.pool_for_tests())
            .await?;
            assert!(present, "{table}.embed_text must exist");
        }

        // `proxima_code` carries no owner org column. Keystone gate for the
        // flavor DDL-drop migration — a missed column would silently keep org
        // in the flavor schema and pass every check above.
        let org_cols: (i64,) = sqlx::query_as(
            "SELECT count(*)::bigint FROM information_schema.columns \
             WHERE table_schema = 'proxima_code' AND column_name = ('owner_' || 'org_id')",
        )
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(
            org_cols.0, 0,
            "legacy owner org column must be absent from proxima_code after Owner=OwnerRef collapse; found {}",
            org_cols.0
        );

        // Idempotency — a second run must not error.
        proxima_code::migrator().run(pg.pool_for_tests()).await?;

        let handle = uuid::Uuid::now_v7();
        let memory_id = uuid::Uuid::now_v7();
        let repo_id = uuid::Uuid::now_v7();
        let owner_id = uuid::Uuid::now_v7();
        sqlx::query(
            "INSERT INTO proxima_core.owners (owner_id, kind)
             VALUES ($1, 'personal') ON CONFLICT DO NOTHING",
        )
        .bind(owner_id)
        .execute(pg.pool_for_tests())
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
             VALUES ($1, 'abstraction', 'proxima-code/code-chunk-v1', $2, $3)",
        )
        .bind(handle)
        .bind(owner_id)
        .bind(memory_id)
        .execute(pg.pool_for_tests())
        .await?;
        let mut hash = [0_u8; 32];
        hash[..16].copy_from_slice(memory_id.as_bytes());
        hash[16..].copy_from_slice(memory_id.as_bytes());
        let content_id: uuid::Uuid = sqlx::query_scalar(
            "INSERT INTO proxima_core.content (owner_id, schema_id, content_hash)
             VALUES ($1, 'proxima-code/code-chunk-v1', $2)
             RETURNING content_id",
        )
        .bind(owner_id)
        .bind(hash.as_slice())
        .fetch_one(pg.pool_for_tests())
        .await?;
        let fact_handle = uuid::Uuid::now_v7();
        let fact_t = uuid::Uuid::now_v7();
        sqlx::query(
            "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
             VALUES ($1, 'fact', 'core/test-fact-v1', $2, $3)",
        )
        .bind(fact_handle)
        .bind(owner_id)
        .bind(fact_t)
        .execute(pg.pool_for_tests())
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.memory (handle, t, kind, owner_id, schema_id)
             VALUES ($1, $2, 'fact', $3, 'core/test-fact-v1')",
        )
        .bind(fact_handle)
        .bind(fact_t)
        .bind(owner_id)
        .execute(pg.pool_for_tests())
        .await?;
        sqlx::query(
            // `sidecar_tables` is not decoration here: the chunk row below
            // is only reachable by forget, erase and export through this
            // stamp, and since the declaration trigger the database refuses
            // the row without it.
            "INSERT INTO proxima_core.memory
                (handle, t, kind, owner_id, schema_id, origins, content_id, sidecar_tables)
             VALUES ($1, $2, 'abstraction', $3, 'proxima-code/code-chunk-v1', ARRAY[$4]::uuid[], $5,
                     ARRAY['proxima_code.code_chunk_v1'])",
        )
        .bind(handle)
        .bind(memory_id)
        .bind(owner_id)
        .bind(fact_t)
        .bind(content_id)
        .execute(pg.pool_for_tests())
        .await?;
        sqlx::query(
            "INSERT INTO proxima_code.code_chunk_v1
                 (t, repo_id, file_path, chunk_index, text, chunk_type,
                  byte_range_start, byte_range_end, line_range_start, line_range_end, state)
             VALUES ($1, $2, 'src/lib.rs', 0, 'fn main() {}', 'file',
                     0, 12, 1, 1, 'Present')",
        )
        .bind(memory_id)
        .bind(repo_id)
        .execute(pg.pool_for_tests())
        .await?;
        let err = sqlx::query(
            "UPDATE proxima_code.code_chunk_v1 SET text = 'rewritten' WHERE t = $1",
        )
        .bind(memory_id)
        .execute(pg.pool_for_tests())
        .await
        .expect_err("code_chunk_v1 content rewrite must be rejected");
        assert!(
            err.to_string().contains("append-only"),
            "expected append-only rejection, got: {err}"
        );

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("flavor_migrations_apply_to_fresh_db failed");
}

/// `OwnerRef` has no id-less kind, so the flavor's owner columns carry the
/// invariant as plain NOT NULL rather than a shape/world CHECK pair.
async fn assert_owner_ref_constraints(
    pool: &sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    for table in ["repos", "repo_ingestion_runs"] {
        for column in ["owner_kind", "owner_id"] {
            let nullable: String = sqlx::query_scalar(
                "SELECT is_nullable
                   FROM information_schema.columns
                  WHERE table_schema = 'proxima_code'
                    AND table_name = $1
                    AND column_name = $2",
            )
            .bind(table)
            .bind(column)
            .fetch_one(pool)
            .await?;
            assert_eq!(
                nullable, "NO",
                "proxima_code.{table}.{column} must be NOT NULL"
            );
        }
        let dead: Vec<String> = sqlx::query_scalar(
            "SELECT constraint_name::text
               FROM information_schema.table_constraints
              WHERE table_schema = 'proxima_code'
                AND table_name = $1
                AND constraint_type = 'CHECK'
                AND constraint_name LIKE '%world%'",
        )
        .bind(table)
        .fetch_all(pool)
        .await?;
        assert!(
            dead.is_empty(),
            "proxima_code.{table} must carry no World CHECK: {dead:?}"
        );
    }
    Ok(())
}

async fn assert_enum_column(
    pool: &sqlx::PgPool,
    schema: &str,
    table: &str,
    column: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let typtype: Option<String> = sqlx::query_scalar(
        "SELECT t.typtype::text
           FROM pg_attribute a
           JOIN pg_class c ON c.oid = a.attrelid
           JOIN pg_namespace n ON n.oid = c.relnamespace
           JOIN pg_type t ON t.oid = a.atttypid
          WHERE n.nspname = $1
            AND c.relname = $2
            AND a.attname = $3
            AND NOT a.attisdropped",
    )
    .bind(schema)
    .bind(table)
    .bind(column)
    .fetch_optional(pool)
    .await?;

    assert_eq!(
        typtype.as_deref(),
        Some("e"),
        "expected {schema}.{table}.{column} to be a SQL enum"
    );
    Ok(())
}

/// The code baseline carries the generator's output verbatim.
///
/// Same pin as core's `generator_output_is_the_migration_text`, on the
/// other side of the flavor boundary: `projection_artifacts` is run over
/// the code flavor's OWN contract, and the baseline has to contain what it
/// emits, character for character.
#[test]
fn generator_output_is_the_code_baseline_text() {
    let artifacts = proxima_storage_pg::projection::projection_artifacts(
        &proxima_code::contract::CODE_FLAVOR_CONTRACT,
    )
    .expect("code artifacts")
    .expect("the code flavor declares a projection");
    let baseline = include_str!("../migrations/20260818000020_v008_baseline.sql");
    for statement in artifacts.forward() {
        assert!(
            baseline.contains(statement),
            "the code baseline does not carry the generator's output verbatim:\n{statement}"
        );
    }
}

/// The code flavor's v0.0.9 migration carries every declaration trigger the
/// generator emits for this flavor, verbatim.
///
/// The sibling of `generated_declaration_triggers_are_the_migration_text`
/// on the other side of the flavor boundary. It is the whole reason the
/// generator takes a flavor id: the DDL lands with the flavor whose
/// migration created the table, and this is what holds that file to it.
///
/// It lands in the additive `20260824000020_v009_declaration_triggers.sql`
/// rather than in the v008 baseline, for the reason the second assertion
/// states: the baseline is frozen, and editing it resets every database that
/// already applied it.
///
/// The shared function is deliberately NOT asserted here — it is core's,
/// defined once in core's `0002`, and a flavor that restated it would be
/// a second declaration of one thing.
#[test]
fn generated_declaration_triggers_are_the_code_migration_text() {
    let registry = proxima_code::schema_registry();
    let mut pg = proxima_storage_pg::PgSidecarRegistry::new();
    proxima_storage_pg::register_core_pg_sidecars(&mut pg);
    proxima_code::register_pg_sidecars(&mut pg);
    let frozen = pg
        .freeze_against(&registry)
        .expect("the code registration freezes");
    let artifacts = frozen
        .declaration_trigger_artifacts(proxima_code::contract::FLAVOR_ID)
        .expect("the code flavor's declaration triggers");
    assert!(
        !artifacts.is_empty(),
        "the code flavor registers memory sidecars, so it has triggers to carry"
    );
    let migration = include_str!("../migrations/20260824000020_v009_declaration_triggers.sql");
    let baseline = include_str!("../migrations/20260818000020_v008_baseline.sql");
    assert!(
        !baseline.contains("assert_memory_declares_sidecar"),
        "the v008 baseline is frozen — declaration integrity ships as the additive \
         20260824000020, never as an edit to a version live databases have already applied"
    );
    for artifact in &artifacts {
        assert!(
            migration.contains(&artifact.forward),
            "the code v0.0.9 migration does not carry the generator's output verbatim:\n{}",
            artifact.forward
        );
    }
}

/// Apply an already-shipped migration the way a live deployment carries it:
/// the file's own bytes, and one ledger row whose checksum comes from the
/// embedded migration rather than from a recomputation here, so this fixture
/// can never disagree with what the compatibility checks compare against.
///
/// Core and the flavor get one function each rather than a parameterised one
/// because the ledger table is not the same table: the flavor keeps its rows
/// in `public._sqlx_migrations_proxima_code`, since a baseline that drops
/// `proxima_code` must not take its own ledger with it. Passing the table in
/// would make two literal statements into dynamic SQL for no gain.
async fn seed_applied_core(
    pool: &sqlx::PgPool,
    migration: &sqlx::migrate::Migration,
) -> Result<(), Box<dyn std::error::Error>> {
    // SQL-POLICY: fixed-fragment — the migration's own embedded text, the
    // same bytes the migrator would execute.
    sqlx::raw_sql(migration.sql.clone()).execute(pool).await?;
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
    .bind(migration.version)
    .bind(migration.description.as_ref())
    .bind(migration.checksum.as_ref())
    .execute(pool)
    .await?;
    Ok(())
}

/// The flavor half of [`seed_applied_core`], against the flavor's own ledger.
async fn seed_applied_code(
    pool: &sqlx::PgPool,
    migration: &sqlx::migrate::Migration,
) -> Result<(), Box<dyn std::error::Error>> {
    // SQL-POLICY: fixed-fragment — the migration's own embedded text, the
    // same bytes the migrator would execute.
    sqlx::raw_sql(migration.sql.clone()).execute(pool).await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS public._sqlx_migrations_proxima_code (
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
        "INSERT INTO public._sqlx_migrations_proxima_code
             (version, description, success, checksum, execution_time)
         VALUES ($1, $2, true, $3, 0)",
    )
    .bind(migration.version)
    .bind(migration.description.as_ref())
    .bind(migration.checksum.as_ref())
    .execute(pool)
    .await?;
    Ok(())
}

fn only(migrator: &sqlx::migrate::Migrator, version: i64) -> &sqlx::migrate::Migration {
    migrator
        .iter()
        .find(|migration| migration.version == version)
        .expect("the embedded set carries this version")
}

/// A v0.0.8 database with the code flavor linked upgrades through v0.0.11 in
/// place, on both sides of the flavor boundary.
///
/// The sibling of core's `a_v008_database_upgrades_to_v011_in_place`. It is
/// asserted separately because the two ledgers are separate tables and the
/// flavor's declaration triggers live in the flavor's own migration: core
/// upgrading cleanly says nothing about whether this one does.
#[tokio::test]
async fn a_v008_code_database_upgrades_to_v011_in_place() {
    let db_name = unique_db_name("proxima_test");
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        let pool = pg.pool_for_tests();

        let core_migrations = proxima_storage_pg::core_migrator();
        let flavor_migrations = proxima_code::migrator();
        seed_applied_core(pool, only(&core_migrations, 1)).await?;
        seed_applied_code(pool, only(&flavor_migrations, 20_260_818_000_020)).await?;

        pg.run_migrations().await.map_err(|err| {
            format!("a live v0.0.8 database must upgrade in place, not reset: {err}")
        })?;
        flavor_migrations.run(pool).await?;

        let core_versions: Vec<i64> = sqlx::query_scalar(
            "SELECT version FROM public._sqlx_migrations
              WHERE success AND version <= 9999 ORDER BY version",
        )
        .fetch_all(pool)
        .await?;
        assert_eq!(
            core_versions,
            vec![1, 2, 3, 4],
            "core appends its v0.0.9, v0.0.10 and v0.0.11 migrations"
        );

        let flavor_versions: Vec<i64> = sqlx::query_scalar(
            "SELECT version FROM public._sqlx_migrations_proxima_code
              WHERE success ORDER BY version",
        )
        .fetch_all(pool)
        .await?;
        assert_eq!(
            flavor_versions,
            vec![20_260_818_000_020, 20_260_824_000_020],
            "the flavor appends its v0.0.9 rather than re-applying its baseline"
        );

        let registry = proxima_code::schema_registry();
        let mut sidecars = proxima_storage_pg::PgSidecarRegistry::new();
        proxima_storage_pg::register_core_pg_sidecars(&mut sidecars);
        proxima_code::register_pg_sidecars(&mut sidecars);
        let frozen = sidecars.freeze_against(&registry)?;
        proxima_storage_pg::integrity::ensure_declaration_triggers(pool, &frozen)
            .await
            .map_err(|err| {
                format!("the upgraded database must satisfy the boot guardrail: {err}")
            })?;

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("v0.0.8 -> v0.0.11 in-place upgrade failed for the code flavor");
}
