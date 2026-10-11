//! Migration 0025 (`_v031_`) gives every Goal version its prompt text.
//!
//! The fixture is a database standing at the released 0024 schema, written the
//! way the released binary wrote it: Goal rows without a `text` column and the
//! replay declarations of `goal_write/replay.rs`. The upgrade is then the real
//! runner, ledger included.
use std::borrow::Cow;
use std::collections::BTreeMap;

use proxima_pg_testkit::{create_db, db_url, drop_db, split_role_urls};
use proxima_storage_pg::{PgStorage, begin_migration_transaction, core_migrator};
use serde_json::json;
use uuid::Uuid;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// A database with every migration up to 0024 applied and recorded.
async fn database_at_0024(db_name: &str) -> TestResult<PgStorage> {
    create_db(db_name).await?;
    let pg = PgStorage::connect(&db_url(db_name)).await?;
    let (_, platform_url) = split_role_urls(db_name).await?;
    let platform = sqlx::PgPool::connect(&platform_url).await?;
    let mut staged = core_migrator();
    staged.migrations = Cow::Owned(
        staged
            .iter()
            .filter(|migration| migration.version < 25)
            .cloned()
            .collect(),
    );
    let mut connection = platform.acquire().await?;
    let mut migration = begin_migration_transaction(&mut connection).await?;
    staged.run(migration.as_mut()).await?;
    migration.commit().await?;
    drop(connection);
    platform.close().await;
    assert!(
        !super::column_exists(&pg, "goal", "text").await,
        "the fixture stands at 0024, before the column exists"
    );
    Ok(pg)
}

/// A `UUIDv7` minted at an exact millisecond, so a series' order is the order
/// the fixture says and not whatever the clock gave.
fn t_at(millis: u64) -> Uuid {
    Uuid::new_v7(uuid::Timestamp::from_unix(
        uuid::NoContext,
        millis / 1000,
        u32::try_from(millis % 1000).expect("below 1000") * 1_000_000,
    ))
}

/// One Goal version as the 0024 binary left it: a lifecycle state and,
/// maybe, the declaration it recorded.
struct Version {
    state: &'static str,
    declaration: Option<serde_json::Value>,
}

fn version(state: &'static str, declaration: Option<serde_json::Value>) -> Version {
    Version { state, declaration }
}

fn draft(text: &serde_json::Value) -> serde_json::Value {
    json!({
        "schema_id": "core/simple-text-v1", "schema_version": 1, "title": "t",
        "text": text, "payload": null, "sidecar": null, "state": "Active",
        "topology": null, "wake": null, "supersedes_goal_id": null, "authorship": "user",
    })
}

fn payload(text: &serde_json::Value) -> serde_json::Value {
    json!({
        "schema_id": "core/simple-text-v1", "schema_version": 1, "title": "t",
        "text": text, "payload": null, "sidecar": null,
    })
}

fn create(text: impl Into<serde_json::Value>) -> serde_json::Value {
    json!({"version": 1, "verb": "create", "draft": draft(&text.into())})
}

fn modify(text: impl Into<serde_json::Value>) -> serde_json::Value {
    json!({
        "version": 1, "verb": "modify", "prior_goal_id": Uuid::nil(),
        "replacement": payload(&text.into()), "wake": {"mode": "carry"},
        "authorship": "user", "evidence": null,
    })
}

fn decompose_child(text: &str) -> serde_json::Value {
    json!({
        "version": 1, "verb": "decompose_child", "parent_goal_id": Uuid::nil(),
        "authorship": "user", "topology": null, "child_index": 0,
        "payload": payload(&json!(text)), "evidence": [], "wake": null,
    })
}

fn transition(next_state: &str) -> serde_json::Value {
    json!({
        "version": 1, "verb": "transition", "prior_goal_id": Uuid::nil(),
        "next_state": next_state, "authorship": "user",
    })
}

fn achieve() -> serde_json::Value {
    json!({
        "version": 1, "verb": "achieve", "prior_goal_id": Uuid::nil(),
        "authorship": "user", "evidence": [],
    })
}

/// Write one series at 0024: its versions `step` ms apart from `start`,
/// returning each version's `t`.
async fn seed_series(
    pool: &sqlx::PgPool,
    owner: Uuid,
    start: u64,
    versions: &[Version],
) -> Result<Vec<Uuid>, sqlx::Error> {
    let handle = Uuid::now_v7();
    let ts: Vec<Uuid> = (0..versions.len())
        .map(|index| t_at(start + 20 * index as u64))
        .collect();
    sqlx::query(
        "INSERT INTO proxima_core.goal_head (handle, schema_id, owner_id, t)
         VALUES ($1, 'core/simple-text-v1', $2, $3)",
    )
    .bind(handle)
    .bind(owner)
    .bind(ts[0])
    .execute(pool)
    .await?;
    for (version, t) in versions.iter().zip(&ts) {
        insert_version(pool, handle, owner, *t, version).await?;
    }
    sqlx::query("UPDATE proxima_core.goal_head SET t = $2 WHERE handle = $1")
        .bind(handle)
        .bind(ts[ts.len() - 1])
        .execute(pool)
        .await?;
    Ok(ts)
}

/// One Goal row of `owner` at `t` in the series `handle`, with its
/// declaration. The 0024 schema checks neither the row's place in the series
/// nor that its owner is the head's.
async fn insert_version(
    pool: &sqlx::PgPool,
    handle: Uuid,
    owner: Uuid,
    t: Uuid,
    version: &Version,
) -> Result<(), sqlx::Error> {
    // A closed Goal names the Fact that closed it.
    let close_fact_t = if matches!(version.state, "Achieved" | "Abandoned") {
        Some(seed_fact(pool, owner).await?)
    } else {
        None
    };
    sqlx::query(
        "INSERT INTO proxima_core.goal
             (handle, t, owner_id, title, state, request_id, close_fact_t)
         VALUES ($1, $2, $3, 'fixture', $4::proxima_core.goal_state, $5, $6)",
    )
    .bind(handle)
    .bind(t)
    .bind(owner)
    .bind(version.state)
    .bind(Uuid::now_v7().to_string())
    .bind(close_fact_t)
    .execute(pool)
    .await?;
    if let Some(declaration) = &version.declaration {
        sqlx::query(
            "INSERT INTO proxima_core.goal_replay_declaration (goal_t, declaration, edge_count)
             VALUES ($1, $2, 0)",
        )
        .bind(t)
        .bind(declaration)
        .execute(pool)
        .await?;
    }
    Ok(())
}

async fn seed_fact(pool: &sqlx::PgPool, owner: Uuid) -> Result<Uuid, sqlx::Error> {
    let handle = Uuid::now_v7();
    let t = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
         VALUES ($1, 'fact', 'test/fact-v1', $2, $3)",
    )
    .bind(handle)
    .bind(owner)
    .bind(t)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.memory (handle, t, kind, owner_id, schema_id)
         VALUES ($1, $2, 'fact', $3, 'test/fact-v1')",
    )
    .bind(handle)
    .bind(t)
    .bind(owner)
    .execute(pool)
    .await?;
    Ok(t)
}

async fn seed_owner(pool: &sqlx::PgPool) -> Result<Uuid, sqlx::Error> {
    let owner = Uuid::now_v7();
    sqlx::query("INSERT INTO proxima_core.owners (owner_id, kind) VALUES ($1, 'personal')")
        .bind(owner)
        .execute(pool)
        .await?;
    Ok(owner)
}

async fn stored_texts(pool: &sqlx::PgPool) -> Result<BTreeMap<Uuid, Option<String>>, sqlx::Error> {
    Ok(
        sqlx::query_as::<_, (Uuid, Option<String>)>("SELECT t, text FROM proxima_core.goal")
            .fetch_all(pool)
            .await?
            .into_iter()
            .collect(),
    )
}

/// The rule the migration states: a version's text is the latest declared,
/// non-blank text among the versions of its series up to it. Series are
/// interleaved in time on purpose, so a leak across handles shows.
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn each_version_takes_the_latest_declared_text_of_its_own_series()
-> Result<(), Box<dyn std::error::Error>> {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    let result: TestResult = async {
        let pg = database_at_0024(&db_name).await?;
        let pool = pg.pool_for_tests();
        let owner = seed_owner(pool).await?;
        let base = 1_700_000_000_000;

        let mut expected: BTreeMap<Uuid, Option<String>> = BTreeMap::new();
        let mut seed = async |offset: u64,
                              versions: Vec<Version>,
                              texts: Vec<Option<&str>>|
               -> Result<(), sqlx::Error> {
            assert_eq!(versions.len(), texts.len(), "the fixture is complete");
            let ts = seed_series(pool, owner, base + offset, &versions).await?;
            expected.extend(
                ts.into_iter()
                    .zip(texts.into_iter().map(|text| text.map(str::to_owned))),
            );
            Ok(())
        };

        // A full life: a modify changes the text for itself and what follows,
        // never for what came before.
        seed(
            0,
            vec![
                version("Active", Some(create("alpha"))),
                version("Paused", Some(transition("Paused"))),
                version("Active", Some(transition("Active"))),
                version("Active", Some(modify("beta"))),
                version("Paused", Some(transition("Paused"))),
                version("Achieved", Some(achieve())),
            ],
            vec![
                Some("alpha"),
                Some("alpha"),
                Some("alpha"),
                Some("beta"),
                Some("beta"),
                Some("beta"),
            ],
        )
        .await?;

        // A blank or non-string text declares nothing: the earlier text
        // stands. A decoy `text` on a verb that has none is not read.
        seed(
            10,
            vec![
                version("Active", Some(create("gamma"))),
                version("Active", Some(modify("   "))),
                version("Active", Some(modify(7))),
                version("Paused", {
                    let mut decoy = transition("Paused");
                    decoy["text"] = json!("decoy");
                    decoy["draft"] = json!({"text": "decoy"});
                    decoy["replacement"] = json!({"text": "decoy"});
                    Some(decoy)
                }),
            ],
            vec![Some("gamma"), Some("gamma"), Some("gamma"), Some("gamma")],
        )
        .await?;

        // The first versions of the series have no declaration (written
        // before v0.0.13): nothing is guessed for them, a later modify
        // declares from its own version on.
        seed(
            5,
            vec![
                version("Active", None),
                version("Paused", None),
                version("Active", Some(modify("delta"))),
                version("Paused", Some(transition("Paused"))),
            ],
            vec![None, None, Some("delta"), Some("delta")],
        )
        .await?;

        // A series with no declaration at all.
        seed(
            15,
            vec![version("Active", None), version("Paused", None)],
            vec![None, None],
        )
        .await?;

        // A decomposed child declares its text under `payload`.
        seed(
            25,
            vec![
                version("Active", Some(decompose_child("epsilon"))),
                version("Paused", Some(transition("Paused"))),
            ],
            vec![Some("epsilon"), Some("epsilon")],
        )
        .await?;
        assert_eq!(expected.len(), 18);

        // The released runner takes the database from 0024 to head.
        super::apply_current_migrations(&pg).await?;

        assert_eq!(stored_texts(pool).await?, expected);
        assert!(super::column_exists(&pg, "goal", "text").await);
        super::assert_current_markers(pool).await?;
        super::assert_current_schema(pool).await?;

        // Running the statement again changes nothing; it is also what a
        // restore of the 0024 data followed by the upgrade would run.
        super::apply_core_migrations(pool, 25..=25).await?;
        assert_eq!(stored_texts(pool).await?, expected);

        // The backfill turned the append-only guard off for one statement.
        let refused = sqlx::query("UPDATE proxima_core.goal SET title = title")
            .execute(pool)
            .await
            .expect_err("the Goal table must stay append-only");
        assert!(refused.to_string().contains("append-only"), "{refused}");
        let refused = sqlx::query("UPDATE proxima_core.goal SET text = 'rewritten'")
            .execute(pool)
            .await
            .expect_err("the text must not be rewritable after the backfill");
        assert!(refused.to_string().contains("append-only"), "{refused}");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}

/// Before 0025 a raw insert could put a row of owner B into owner A's series:
/// the foreign key checks the handle only, and the row policy checks the row's
/// own owner. The backfill reads each owner's declarations apart, so A's text
/// never lands in B's row, where B's export and reads would show it, and B's
/// text never lands in A's.
#[tokio::test]
async fn a_row_of_another_owner_in_the_series_never_takes_its_text()
-> Result<(), Box<dyn std::error::Error>> {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    let result: TestResult = async {
        let pg = database_at_0024(&db_name).await?;
        let pool = pg.pool_for_tests();
        let owner_a = seed_owner(pool).await?;
        let owner_b = seed_owner(pool).await?;
        let base = 1_700_000_000_000;

        let a_ts = seed_series(
            pool,
            owner_a,
            base,
            &[
                version("Active", Some(create("secret"))),
                version("Paused", Some(transition("Paused"))),
            ],
        )
        .await?;
        let handle: Uuid = sqlx::query_scalar("SELECT handle FROM proxima_core.goal WHERE t = $1")
            .bind(a_ts[0])
            .fetch_one(pool)
            .await?;
        let mut expected: BTreeMap<Uuid, Option<String>> = a_ts
            .into_iter()
            .map(|t| (t, Some("secret".to_owned())))
            .collect();

        // B's rows interleave with A's later ones.
        for (offset, owner, version, text) in [
            (30, owner_b, version("Active", None), None),
            (
                40,
                owner_b,
                version("Active", Some(modify("bravo"))),
                Some("bravo"),
            ),
            (
                50,
                owner_a,
                version("Active", Some(transition("Active"))),
                Some("secret"),
            ),
            (
                60,
                owner_b,
                version("Paused", Some(transition("Paused"))),
                Some("bravo"),
            ),
        ] {
            let t = t_at(base + offset);
            insert_version(pool, handle, owner, t, &version).await?;
            expected.insert(t, text.map(str::to_owned));
        }

        super::apply_current_migrations(&pg).await?;
        assert_eq!(stored_texts(pool).await?, expected);
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}

/// Blank is one definition in the CHECK and in the backfill: nothing left after
/// removing the six ASCII whitespace characters. A declaration whose text is
/// blank in that sense declares nothing, so its version inherits the text
/// declared before it, or stays unknown when there is none. A text with any
/// other character in it, `U+00A0` and the other Unicode spaces included, is
/// not blank to the database and is stored as declared.
#[tokio::test]
async fn an_ascii_whitespace_declaration_declares_nothing() -> Result<(), Box<dyn std::error::Error>>
{
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    let result: TestResult = async {
        let pg = database_at_0024(&db_name).await?;
        let pool = pg.pool_for_tests();
        let owner = seed_owner(pool).await?;
        let base = 1_700_000_000_000;
        let every_kind = " \t\n\r\x0b\x0c";

        let mut expected: BTreeMap<Uuid, Option<String>> = BTreeMap::new();
        let mut seed = async |offset: u64, versions: Vec<Version>, texts: Vec<Option<&str>>| {
            assert_eq!(versions.len(), texts.len(), "the fixture is complete");
            let ts = seed_series(pool, owner, base + offset, &versions).await?;
            expected.extend(
                ts.into_iter()
                    .zip(texts.into_iter().map(|text| text.map(str::to_owned))),
            );
            Ok::<(), sqlx::Error>(())
        };

        // Earlier text stands under a tab, a newline, a vertical tab, a form
        // feed and all six together.
        seed(
            0,
            vec![
                version("Active", Some(create("alpha"))),
                version("Active", Some(modify("\t"))),
                version("Active", Some(modify("\n"))),
                version("Active", Some(modify("\u{b}"))),
                version("Active", Some(modify("\u{c}"))),
                version("Active", Some(modify(every_kind))),
                version("Paused", Some(transition("Paused"))),
            ],
            vec![Some("alpha"); 7],
        )
        .await?;

        // Nothing declared before a blank one: unknown, until a real text.
        seed(
            10,
            vec![
                version("Active", Some(create("\t"))),
                version("Paused", Some(transition("Paused"))),
                version("Active", Some(modify("\r\n"))),
                version("Active", Some(modify("real"))),
                version("Paused", Some(transition("Paused"))),
            ],
            vec![None, None, None, Some("real"), Some("real")],
        )
        .await?;

        // Not ASCII whitespace, so not blank: kept as declared, padding and
        // all.
        seed(
            15,
            vec![
                version("Active", Some(create("\u{a0}"))),
                version("Active", Some(modify(" \u{2003}\t"))),
                version("Paused", Some(transition("Paused"))),
            ],
            vec![Some("\u{a0}"), Some(" \u{2003}\t"), Some(" \u{2003}\t")],
        )
        .await?;

        super::apply_current_migrations(&pg).await?;
        assert_eq!(stored_texts(pool).await?, expected);
        super::assert_current_markers(pool).await?;
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}

/// The upgrade of a database with no Goal at all (a fresh install runs the
/// same file) and its repeat.
#[tokio::test]
async fn an_empty_database_takes_the_migration_and_a_repeat()
-> Result<(), Box<dyn std::error::Error>> {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    let result: TestResult = async {
        let pg = database_at_0024(&db_name).await?;
        super::apply_current_migrations(&pg).await?;
        super::apply_core_migrations(pg.pool_for_tests(), 25..=25).await?;
        super::assert_current_schema(pg.pool_for_tests()).await?;
        let goals: i64 = sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.goal")
            .fetch_one(pg.pool_for_tests())
            .await?;
        assert_eq!(goals, 0);
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}

/// Migration 0025's column and constraint, as the file writes them.
const RESTORE: &str = r"
    ALTER TABLE proxima_core.goal ALTER COLUMN text DROP NOT NULL;
    ALTER TABLE proxima_core.goal DROP CONSTRAINT IF EXISTS goal_text_nonblank_chk;
    ALTER TABLE proxima_core.goal ADD CONSTRAINT goal_text_nonblank_chk
        CHECK (text IS NULL OR length(btrim(text, E' \t\n\r\x0b\f')) > 0)";

const MISSING_OR_INCORRECT: &str = "goal.text non-blank check is missing or incorrect";

const TRIGGER_MISSING_OR_INCORRECT: &str =
    "goal.text carry-forward trigger goal_text_carried is missing or incorrect";

/// Each damage and the marker message it must produce.
const DAMAGE: [(&str, &str); 15] = [
    (
        "ALTER TABLE proxima_core.goal DROP CONSTRAINT goal_text_nonblank_chk",
        MISSING_OR_INCORRECT,
    ),
    (
        r"ALTER TABLE proxima_core.goal DROP CONSTRAINT goal_text_nonblank_chk;
          ALTER TABLE proxima_core.goal ADD CONSTRAINT goal_text_nonblank_chk
              CHECK (text IS NULL OR length(text) > 0)",
        MISSING_OR_INCORRECT,
    ),
    // The spaces-only `btrim(text)` is not the one definition of blank.
    (
        r"ALTER TABLE proxima_core.goal DROP CONSTRAINT goal_text_nonblank_chk;
          ALTER TABLE proxima_core.goal ADD CONSTRAINT goal_text_nonblank_chk
              CHECK (text IS NULL OR length(btrim(text)) > 0)",
        MISSING_OR_INCORRECT,
    ),
    (
        r"ALTER TABLE proxima_core.goal DROP CONSTRAINT goal_text_nonblank_chk;
          ALTER TABLE proxima_core.goal ADD CONSTRAINT goal_text_nonblank_chk
              CHECK (text IS NULL OR length(btrim(text, E' \t\n\r\x0b\f')) > 0) NOT VALID",
        MISSING_OR_INCORRECT,
    ),
    (
        "ALTER TABLE proxima_core.goal ALTER COLUMN text SET NOT NULL",
        "goal.text must be a nullable text column",
    ),
    // The carry-forward trigger: gone, switched off, with a body that carries
    // nothing, a definer, an unpinned search_path, or wired to another event
    // or time.
    (
        "DROP TRIGGER goal_text_carried ON proxima_core.goal",
        TRIGGER_MISSING_OR_INCORRECT,
    ),
    (
        "ALTER TABLE proxima_core.goal DISABLE TRIGGER goal_text_carried",
        TRIGGER_MISSING_OR_INCORRECT,
    ),
    (
        r"CREATE OR REPLACE FUNCTION proxima_core.goal_text_carried()
          RETURNS trigger LANGUAGE plpgsql
          SET search_path = pg_catalog, proxima_core, pg_temp
          AS $$ BEGIN RETURN NEW; END $$",
        TRIGGER_MISSING_OR_INCORRECT,
    ),
    // The carry check alone, against the version just before the new `t`: a
    // version slipped between two others would pass it and leave the later one
    // with a predecessor whose text it does not carry.
    (
        r"CREATE OR REPLACE FUNCTION proxima_core.goal_text_carried()
          RETURNS trigger LANGUAGE plpgsql
          SET search_path = pg_catalog, proxima_core, pg_temp
          AS $$
          DECLARE
              prior_state proxima_core.goal_state;
              prior_text text;
          BEGIN
              SELECT g.state, g.text INTO prior_state, prior_text
                FROM proxima_core.goal g
               WHERE g.handle = NEW.handle AND g.t < NEW.t
               ORDER BY g.t DESC LIMIT 1;
              IF FOUND
                 AND prior_state IS DISTINCT FROM NEW.state
                 AND NEW.text IS DISTINCT FROM prior_text
              THEN
                  RAISE EXCEPTION 'carry' USING ERRCODE = '23514';
              END IF;
              RETURN NEW;
          END $$",
        TRIGGER_MISSING_OR_INCORRECT,
    ),
    // Order and carry, but no series lock: two concurrent inserts on one
    // series would both judge themselves against the same predecessor.
    (
        r"CREATE OR REPLACE FUNCTION proxima_core.goal_text_carried()
          RETURNS trigger LANGUAGE plpgsql
          SET search_path = pg_catalog, proxima_core, pg_temp
          AS $$
          DECLARE
              prior_t uuid;
              prior_state proxima_core.goal_state;
              prior_text text;
          BEGIN
              SELECT g.t, g.state, g.text INTO prior_t, prior_state, prior_text
                FROM proxima_core.goal g
               WHERE g.handle = NEW.handle
               ORDER BY g.t DESC LIMIT 1;
              IF FOUND AND prior_t > NEW.t THEN
                  RAISE EXCEPTION 'order' USING ERRCODE = '23514';
              END IF;
              IF FOUND
                 AND prior_state IS DISTINCT FROM NEW.state
                 AND NEW.text IS DISTINCT FROM prior_text
              THEN
                  RAISE EXCEPTION 'carry' USING ERRCODE = '23514';
              END IF;
              RETURN NEW;
          END $$",
        TRIGGER_MISSING_OR_INCORRECT,
    ),
    // A lock on the head instead of a write to the owner's head: it fails
    // open for a scope that cannot see the head, and a stale REPEATABLE READ
    // snapshot passes it.
    (
        r"CREATE OR REPLACE FUNCTION proxima_core.goal_text_carried()
          RETURNS trigger LANGUAGE plpgsql
          SET search_path = pg_catalog, proxima_core, pg_temp
          AS $$
          DECLARE
              prior_t uuid;
              prior_state proxima_core.goal_state;
              prior_text text;
          BEGIN
              PERFORM 1 FROM proxima_core.goal_head h
               WHERE h.handle = NEW.handle FOR UPDATE;
              SELECT g.t, g.state, g.text INTO prior_t, prior_state, prior_text
                FROM proxima_core.goal g
               WHERE g.handle = NEW.handle
               ORDER BY g.t DESC LIMIT 1;
              IF FOUND AND prior_t > NEW.t THEN
                  RAISE EXCEPTION 'order' USING ERRCODE = '23514';
              END IF;
              IF FOUND
                 AND prior_state IS DISTINCT FROM NEW.state
                 AND NEW.text IS DISTINCT FROM prior_text
              THEN
                  RAISE EXCEPTION 'carry' USING ERRCODE = '23514';
              END IF;
              RETURN NEW;
          END $$",
        TRIGGER_MISSING_OR_INCORRECT,
    ),
    (
        "ALTER FUNCTION proxima_core.goal_text_carried() SECURITY DEFINER",
        TRIGGER_MISSING_OR_INCORRECT,
    ),
    (
        "ALTER FUNCTION proxima_core.goal_text_carried()
             SET search_path = pg_catalog, proxima_core",
        TRIGGER_MISSING_OR_INCORRECT,
    ),
    (
        r"DROP TRIGGER goal_text_carried ON proxima_core.goal;
          CREATE TRIGGER goal_text_carried AFTER INSERT ON proxima_core.goal
              FOR EACH ROW EXECUTE FUNCTION proxima_core.goal_text_carried()",
        TRIGGER_MISSING_OR_INCORRECT,
    ),
    (
        r"DROP TRIGGER goal_text_carried ON proxima_core.goal;
          CREATE TRIGGER goal_text_carried BEFORE INSERT OR UPDATE ON proxima_core.goal
              FOR EACH ROW EXECUTE FUNCTION proxima_core.goal_text_carried()",
        TRIGGER_MISSING_OR_INCORRECT,
    ),
];

/// The boot guardrail names a database whose `goal.text` was damaged after
/// the ledger said 0025 was applied.
#[tokio::test]
async fn the_boot_guardrail_rejects_a_damaged_goal_text_column()
-> Result<(), Box<dyn std::error::Error>> {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    let result: TestResult = async {
        let pg = database_at_0024(&db_name).await?;
        super::apply_current_migrations(&pg).await?;
        let admin = pg.pool_for_tests();
        super::assert_current_markers(admin).await?;
        let platform = super::marker_platform_pool(admin).await?;

        for (sql, message) in DAMAGE {
            let mut tx = platform.begin().await?;
            sqlx::raw_sql(sql).execute(&mut *tx).await?;
            tx.commit().await?;
            let error = super::assert_current_markers(admin)
                .await
                .expect_err("a damaged goal.text must fail the boot guardrail");
            assert!(error.to_string().contains(message), "{sql}: {error}");
            // Put the schema back for the next damage: the constraint as the
            // file writes it, then the file's own idempotent function and
            // trigger (the repair a re-run of 0025 is).
            let mut tx = platform.begin().await?;
            sqlx::raw_sql(RESTORE).execute(&mut *tx).await?;
            tx.commit().await?;
            super::apply_core_migrations(&platform, 25..=25).await?;
            super::assert_current_markers(admin).await?;
        }
        platform.close().await;
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}
