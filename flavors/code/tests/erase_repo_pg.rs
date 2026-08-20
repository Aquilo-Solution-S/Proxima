mod common;

use common::{migrated_db, test_owner};
use proxima_code::CodeFlavorStore;
use proxima_code::CommitV1;
use proxima_code::RepoScope;
use proxima_code::testkit::{erase_repo, register_repo};
use proxima_core::{FactPayload, Owner};
use proxima_pg_testkit::drop_db;
use uuid::Uuid;

async fn insert_repo_commit_with_test_request(
    pool: &sqlx::PgPool,
    owner: &Owner,
    repo_id: Uuid,
    memory_id: Uuid,
) -> Result<(), sqlx::Error> {
    let owner_id = owner.stored_owner_id();
    let handle = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxima_core.owners (owner_id, kind)
         VALUES ($1, 'personal') ON CONFLICT DO NOTHING",
    )
    .bind(owner_id)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
         VALUES ($1, 'fact', $2, $3, $4)",
    )
    .bind(handle)
    .bind(CommitV1::SCHEMA_ID)
    .bind(owner_id)
    .bind(memory_id)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.memory (handle, t, kind, owner_id, schema_id)
         VALUES ($1, $2, 'fact', $3, $4)",
    )
    .bind(handle)
    .bind(memory_id)
    .bind(owner_id)
    .bind(CommitV1::SCHEMA_ID)
    .execute(pool)
    .await?;

    sqlx::query(
        "INSERT INTO proxima_code.commit_v1
            (t, repo_id, sha, parents, author_name, author_email,
             author_time, committer_name, committer_email, committer_time, message)
         VALUES ($1, $2, 'abc1234', ARRAY[]::text[], 'A', 'a@example.com',
             now(), 'A', 'a@example.com', now(), 'fixture')",
    )
    .bind(memory_id)
    .bind(repo_id)
    .execute(pool)
    .await?;

    sqlx::query(
        "INSERT INTO proxima_code.test_requested_v1
            (t, repo_id, title, instructions, test_key, criteria_count)
         VALUES ($1, $2, 'bug 2', 'delete sidecar', 'bug-2', 1)",
    )
    .bind(memory_id)
    .bind(repo_id)
    .execute(pool)
    .await?;

    sqlx::query(
        "INSERT INTO proxima_code.test_requested_criterion_v1
            (test_requested_memory_id, criterion_index, criterion_key,
             description, required, verifier_kind)
         VALUES ($1, 0, 'c', 'criterion', true, 'reviewer_only')",
    )
    .bind(memory_id)
    .execute(pool)
    .await?;

    Ok(())
}

#[tokio::test]
async fn erase_repo_deletes_repo_rows_and_preserves_other_repos() {
    let (db_name, pg) = migrated_db().await;
    let result = exercise_repo_erase(pg.pool_for_tests()).await;
    let _ = drop_db(&db_name).await;
    result.expect("erase_repo_deletes_repo_rows_and_preserves_other_repos failed");
}

async fn exercise_repo_erase(pool: &sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let owner = test_owner();
    let repo_id = Uuid::now_v7();
    let other_repo_id = Uuid::now_v7();
    let repo_path = format!("/tmp/proxima-erase-repo-{repo_id}");
    register_repo(
        pool,
        &owner,
        repo_id,
        &repo_path,
        "erase repo fixture",
        &RepoScope::default(),
    )
    .await?;
    register_repo(
        pool,
        &owner,
        other_repo_id,
        &format!("/tmp/proxima-erase-repo-keep-{other_repo_id}"),
        "keep repo fixture",
        &RepoScope::default(),
    )
    .await?;

    let memory_id = Uuid::now_v7();
    insert_repo_commit_with_test_request(pool, &owner, repo_id, memory_id).await?;
    let other_memory_id = Uuid::now_v7();
    insert_repo_commit_with_test_request(pool, &owner, other_repo_id, other_memory_id).await?;
    let owner_id = owner.stored_owner_id();
    sqlx::query(
        "INSERT INTO proxima_core.sketch (t, owner_id, kind, text)
         VALUES ($1, $2, 'fact', 'erase-repo-target'), ($3, $2, 'fact', 'erase-repo-keep')",
    )
    .bind(memory_id)
    .bind(owner_id)
    .bind(other_memory_id)
    .execute(pool)
    .await?;

    let store = CodeFlavorStore::from_backend_pool_for_tests(pool.clone());
    let receipt = erase_repo(&store, &owner, repo_id).await?;
    assert_eq!(receipt.repo_id, repo_id);
    assert_eq!(receipt.memories_deleted, 1);
    assert_eq!(receipt.cold_objects_pending, 0);
    assert!(receipt.repo_record_deleted);

    assert_repo_erased(pool, repo_id, memory_id).await?;
    assert_other_repo_preserved(pool, other_repo_id, other_memory_id).await?;
    assert_repo_rebuild_allowed(pool, &owner, repo_id, &repo_path).await
}

async fn assert_repo_erased(
    pool: &sqlx::PgPool,
    repo_id: Uuid,
    memory_id: Uuid,
) -> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*)::bigint FROM proxima_code.test_requested_v1 WHERE t = $1",
        )
        .bind(memory_id)
        .fetch_one(pool)
        .await?,
        0_i64
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*)::bigint FROM proxima_code.test_requested_criterion_v1 WHERE test_requested_memory_id = $1",
        )
        .bind(memory_id)
        .fetch_one(pool)
        .await?,
        0_i64
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*)::bigint FROM proxima_code.commit_v1 WHERE t = $1",
        )
        .bind(memory_id)
        .fetch_one(pool)
        .await?,
        0_i64
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*)::bigint FROM proxima_core.memory WHERE t = $1",
        )
        .bind(memory_id)
        .fetch_one(pool)
        .await?,
        0_i64
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*)::bigint FROM proxima_code.repos WHERE repo_id = $1",
        )
        .bind(repo_id)
        .fetch_one(pool)
        .await?,
        0_i64
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*)::bigint FROM proxima_core.sketch WHERE t = $1",
        )
        .bind(memory_id)
        .fetch_one(pool)
        .await?,
        0_i64
    );
    Ok(())
}

async fn assert_other_repo_preserved(
    pool: &sqlx::PgPool,
    repo_id: Uuid,
    memory_id: Uuid,
) -> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*)::bigint FROM proxima_core.memory WHERE t = $1",
        )
        .bind(memory_id)
        .fetch_one(pool)
        .await?,
        1_i64
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*)::bigint FROM proxima_code.repos WHERE repo_id = $1",
        )
        .bind(repo_id)
        .fetch_one(pool)
        .await?,
        1_i64
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*)::bigint FROM proxima_core.sketch WHERE t = $1",
        )
        .bind(memory_id)
        .fetch_one(pool)
        .await?,
        1_i64
    );
    Ok(())
}

async fn assert_repo_rebuild_allowed(
    pool: &sqlx::PgPool,
    owner: &Owner,
    repo_id: Uuid,
    repo_path: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    register_repo(
        pool,
        owner,
        repo_id,
        repo_path,
        "rebuilt repo fixture",
        &RepoScope::default(),
    )
    .await?;
    let rebuilt_memory_id = Uuid::now_v7();
    insert_repo_commit_with_test_request(pool, owner, repo_id, rebuilt_memory_id).await?;
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*)::bigint FROM proxima_core.memory WHERE t = $1",
        )
        .bind(rebuilt_memory_id)
        .fetch_one(pool)
        .await?,
        1_i64
    );
    Ok(())
}

/// The kernel's `code_repo_erase` collected `t`s from five sidecars —
/// `file_revision_v1`, `code_chunk_v1`, `commit_v1`, `commit_summary_v1`,
/// `test_requested_v1` — and deleted rows from those plus two detail
/// tables. Eleven declared sidecars were reachable by no statement in it.
///
/// Three of the eleven are worse than leaks: `acceptance_criteria_v1`,
/// `acceptance_verification_v1` and `work_assignment_v1` hold a plain
/// (non-cascading, non-nulling) foreign key on `proxima_core.memory(t)`
/// through `work_item_memory_id`. Leaving them behind does not leave a
/// stale row, it makes the substrate delete RAISE — which the old code
/// never discovered because it never deleted the work item's memory row
/// either.
///
/// The two the erase must NOT reach are the owner's self-model rows. They
/// carry no `repo_id`, they are what `work_assignment_v1` points at, and
/// they outlive every repository the owner registers.
#[tokio::test]
async fn erase_reaches_the_work_item_sidecars_and_spares_the_owner_self_model() {
    let (db_name, pg) = migrated_db().await;
    let result = exercise_work_item_erase(pg.pool_for_tests()).await;
    let _ = drop_db(&db_name).await;
    result.expect("erase_reaches_the_work_item_sidecars_and_spares_the_owner_self_model failed");
}

/// A bare admission for `t`.
///
/// Every row is a Fact of `commit-v1`, including the ones whose sidecar is
/// a Perspective table: this fixture is about which TABLES the erase
/// reaches, and `memory_pin_checks` requires a non-Fact to pin something,
/// which would add a second subject to the test.
async fn insert_memory(pool: &sqlx::PgPool, owner: &Owner, t: Uuid) -> Result<(), sqlx::Error> {
    let owner_id = owner.stored_owner_id();
    let handle = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxima_core.owners (owner_id, kind)
         VALUES ($1, 'personal') ON CONFLICT DO NOTHING",
    )
    .bind(owner_id)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
         VALUES ($1, 'fact', $2, $3, $4)",
    )
    .bind(handle)
    .bind(CommitV1::SCHEMA_ID)
    .bind(owner_id)
    .bind(t)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.memory (handle, t, kind, owner_id, schema_id)
         VALUES ($1, $2, 'fact', $3, $4)",
    )
    .bind(handle)
    .bind(t)
    .bind(owner_id)
    .bind(CommitV1::SCHEMA_ID)
    .execute(pool)
    .await?;
    Ok(())
}

/// Every relation that still holds a row it should not, by name.
///
/// One statement rather than a loop over table names: a loop needs the
/// table name as data, and a table name as data is dynamic SQL. This asks
/// the same question with fixed text and reports which relation failed,
/// which a `count(*) = 0` per table would not.
const SURVIVORS_SQL: &str = "\
SELECT 'work_requested_v1' AS relation
  FROM proxima_code.work_requested_v1 WHERE t = $1
UNION ALL
SELECT 'acceptance_criteria_v1'
  FROM proxima_code.acceptance_criteria_v1 WHERE t = $2
UNION ALL
SELECT 'acceptance_criterion_v1 (cascade)'
  FROM proxima_code.acceptance_criterion_v1 WHERE criteria_memory_id = $2
UNION ALL
SELECT 'acceptance_verification_v1'
  FROM proxima_code.acceptance_verification_v1 WHERE t = $3
UNION ALL
SELECT 'work_assignment_v1'
  FROM proxima_code.work_assignment_v1 WHERE t = $4
UNION ALL
SELECT 'development_perspective_v1'
  FROM proxima_code.development_perspective_v1 WHERE t = $5
UNION ALL
SELECT 'the work item admission'
  FROM proxima_core.memory WHERE t = $1
ORDER BY relation";

/// The five ids the fixture seeds, in the order they are asserted on.
struct WorkItemFixture {
    work_item: Uuid,
    criteria: Uuid,
    verification: Uuid,
    engineer: Uuid,
    assignment: Uuid,
    perspective: Uuid,
}

async fn seed_work_item_fixture(
    pool: &sqlx::PgPool,
    owner: &Owner,
    repo_id: Uuid,
) -> Result<WorkItemFixture, Box<dyn std::error::Error>> {
    register_repo(
        pool,
        owner,
        repo_id,
        &format!("/tmp/proxima-erase-work-item-{repo_id}"),
        "work item fixture",
        &RepoScope::default(),
    )
    .await?;

    let work_item = Uuid::now_v7();
    insert_memory(pool, owner, work_item).await?;
    sqlx::query(
        "INSERT INTO proxima_code.work_requested_v1
            (t, repo_id, title, instructions, request_key)
         VALUES ($1, $2, 'do the thing', 'instructions', 'req-1')",
    )
    .bind(work_item)
    .bind(repo_id)
    .execute(pool)
    .await?;

    let criteria = Uuid::now_v7();
    insert_memory(pool, owner, criteria).await?;
    sqlx::query(
        "INSERT INTO proxima_code.acceptance_criteria_v1
            (t, work_item_memory_id, criteria_count)
         VALUES ($1, $2, 1)",
    )
    .bind(criteria)
    .bind(work_item)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_code.acceptance_criterion_v1
            (criteria_memory_id, criterion_index, criterion_key,
             description, required, verifier_kind)
         VALUES ($1, 0, 'c', 'criterion', true, 'reviewer_only')",
    )
    .bind(criteria)
    .execute(pool)
    .await?;

    let verification = Uuid::now_v7();
    insert_memory(pool, owner, verification).await?;
    sqlx::query(
        "INSERT INTO proxima_code.acceptance_verification_v1
            (t, work_item_memory_id, criterion_key, status, summary, artifact_refs)
         VALUES ($1, $2, 'c', 'passed', 'looks right', ARRAY[]::text[])",
    )
    .bind(verification)
    .bind(work_item)
    .execute(pool)
    .await?;

    // No repo_id: the owner's engineer identity, which the erase must spare.
    let engineer = Uuid::now_v7();
    insert_memory(pool, owner, engineer).await?;
    sqlx::query(
        "INSERT INTO proxima_code.engineer_self_v1 (t, display_name, purpose)
         VALUES ($1, 'engineer', 'writes code')",
    )
    .bind(engineer)
    .execute(pool)
    .await?;

    let assignment = Uuid::now_v7();
    insert_memory(pool, owner, assignment).await?;
    sqlx::query(
        "INSERT INTO proxima_code.work_assignment_v1
            (t, repo_id, work_item_memory_id, target_perspective_memory_id, reason)
         VALUES ($1, $2, $3, $4, 'because')",
    )
    .bind(assignment)
    .bind(repo_id)
    .bind(work_item)
    .bind(engineer)
    .execute(pool)
    .await?;

    let perspective = Uuid::now_v7();
    insert_memory(pool, owner, perspective).await?;
    sqlx::query(
        "INSERT INTO proxima_code.development_perspective_v1
            (t, repo_id, summary, pattern, risk, recommended_posture, confidence)
         VALUES ($1, $2, 's', 'p', 'r', 'rp', 0.5)",
    )
    .bind(perspective)
    .bind(repo_id)
    .execute(pool)
    .await?;

    Ok(WorkItemFixture {
        work_item,
        criteria,
        verification,
        engineer,
        assignment,
        perspective,
    })
}

async fn exercise_work_item_erase(pool: &sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let owner = test_owner();
    let repo_id = Uuid::now_v7();
    let WorkItemFixture {
        work_item,
        criteria,
        verification,
        engineer,
        assignment,
        perspective,
    } = seed_work_item_fixture(pool, &owner, repo_id).await?;

    let store = CodeFlavorStore::from_backend_pool_for_tests(pool.clone());
    let receipt = erase_repo(&store, &owner, repo_id).await?;
    assert!(receipt.repo_record_deleted);
    assert_eq!(
        receipt.memories_deleted, 5,
        "work item, criteria, verification, assignment and perspective; \
         the engineer identity is not the repo's"
    );

    let survivors: Vec<String> = sqlx::query_scalar(SURVIVORS_SQL)
        .bind(work_item)
        .bind(criteria)
        .bind(verification)
        .bind(assignment)
        .bind(perspective)
        .fetch_all(pool)
        .await?;
    assert!(
        survivors.is_empty(),
        "these rows survived a repo erase: {survivors:?}"
    );

    let spared: Vec<String> = sqlx::query_scalar(
        "SELECT 'engineer_self_v1' AS relation
           FROM proxima_code.engineer_self_v1 WHERE t = $1
         UNION ALL
         SELECT 'its admission' FROM proxima_core.memory WHERE t = $1
         ORDER BY relation",
    )
    .bind(engineer)
    .fetch_all(pool)
    .await?;
    assert_eq!(
        spared,
        vec!["engineer_self_v1".to_string(), "its admission".to_string()],
        "the owner's engineer identity is not one repository's row, and sparing \
         the sidecar means sparing its admission"
    );
    Ok(())
}

/// A row in ANOTHER repository pointing at this one's work item.
///
/// Nine `proxima_code` columns reference `proxima_core.memory` outside their
/// own `t`, all `NO ACTION`, and nothing constrains the referencing row's
/// `repo_id` to agree with the repo of the memory it names — the sweep
/// filters each table by its own `repo_id`, so a cross-repo pointer simply
/// survives it. Before the reference closure this ABORTED: repo B's
/// `execution_result_v1` still named repo A's work item when A's admission
/// was deleted, and `execution_result_v1_work_requested_memory_id_fkey`
/// raised. Erasing A was impossible for as long as B held the pointer.
///
/// The semantics now: the pointer is erased with what it points at, and so
/// is the admission behind it. Repo B survives; the row of B's that was a
/// reference into A does not.
#[tokio::test]
async fn a_cross_repo_reference_is_erased_with_what_it_points_at() {
    let (db_name, pg) = migrated_db().await;
    let result = exercise_cross_repo_erase(pg.pool_for_tests()).await;
    let _ = drop_db(&db_name).await;
    result.expect("a_cross_repo_reference_is_erased_with_what_it_points_at failed");
}

async fn exercise_cross_repo_erase(pool: &sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let owner = test_owner();
    let erased_repo = Uuid::now_v7();
    let other_repo = Uuid::now_v7();
    let WorkItemFixture { work_item, .. } =
        seed_work_item_fixture(pool, &owner, erased_repo).await?;
    register_repo(
        pool,
        &owner,
        other_repo,
        &format!("/tmp/proxima-erase-cross-{other_repo}"),
        "the repo that points across",
        &RepoScope::default(),
    )
    .await?;

    // Filed under `other_repo`, reporting on `erased_repo`'s work item.
    let stray_result = Uuid::now_v7();
    insert_memory(pool, &owner, stray_result).await?;
    sqlx::query(
        "INSERT INTO proxima_code.execution_result_v1
            (t, repo_id, work_requested_memory_id, status, summary, artifact_refs)
         VALUES ($1, $2, $3, 'succeeded', 'done elsewhere', ARRAY[]::text[])",
    )
    .bind(stray_result)
    .bind(other_repo)
    .bind(work_item)
    .execute(pool)
    .await?;

    let store = CodeFlavorStore::from_backend_pool_for_tests(pool.clone());
    let receipt = erase_repo(&store, &owner, erased_repo).await?;
    assert!(receipt.repo_record_deleted);
    assert_eq!(
        receipt.memories_deleted, 6,
        "the five rows of the erased repo, plus the other repo's reference into it"
    );

    let survivors: Vec<String> = sqlx::query_scalar(
        "SELECT 'execution_result_v1' AS relation
           FROM proxima_code.execution_result_v1 WHERE t = $1
         UNION ALL
         SELECT 'its admission' FROM proxima_core.memory WHERE t = $1
         ORDER BY relation",
    )
    .bind(stray_result)
    .fetch_all(pool)
    .await?;
    assert!(
        survivors.is_empty(),
        "a reference to erased data is erased with it, admission included: {survivors:?}"
    );
    let other_repo_rows: i64 =
        sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_code.repos WHERE repo_id = $1")
            .bind(other_repo)
            .fetch_one(pool)
            .await?;
    assert_eq!(
        other_repo_rows, 1,
        "the other repository itself is untouched — only its dangling pointer went"
    );
    Ok(())
}

/// `development_perspective_v1.repo_id` is nullable, and the sweep filters
/// `WHERE repo_id = $1`.
///
/// A nullable column silently excluded from a sweep looks exactly like a
/// bug, so the intent is written down here rather than left to be
/// rediscovered: the payload documents `repo_id: None` as "cross-repo
/// observations", which makes a NULL-repo perspective the owner's, not any
/// one repository's — the same standing `engineer_self_v1` has. It goes
/// with the owner, through the compliance erase.
#[tokio::test]
async fn a_perspective_about_no_particular_repo_survives_a_repo_erase() {
    let (db_name, pg) = migrated_db().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pool = pg.pool_for_tests();
        let owner = test_owner();
        let repo_id = Uuid::now_v7();
        let _ = seed_work_item_fixture(pool, &owner, repo_id).await?;

        let cross_repo = Uuid::now_v7();
        insert_memory(pool, &owner, cross_repo).await?;
        sqlx::query(
            "INSERT INTO proxima_code.development_perspective_v1
                (t, repo_id, summary, pattern, risk, recommended_posture, confidence)
             VALUES ($1, NULL, 'about the codebase at large', 'p', 'r', 'rp', 0.5)",
        )
        .bind(cross_repo)
        .execute(pool)
        .await?;

        let store = CodeFlavorStore::from_backend_pool_for_tests(pool.clone());
        erase_repo(&store, &owner, repo_id).await?;

        let survived: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint
               FROM proxima_code.development_perspective_v1 WHERE t = $1",
        )
        .bind(cross_repo)
        .fetch_one(pool)
        .await?;
        assert_eq!(
            survived, 1,
            "a perspective filed under no repository is not one repository's to erase"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("a_perspective_about_no_particular_repo_survives_a_repo_erase failed");
}

/// The closure statement's column list, asked of the database.
///
/// The list is nine columns hand-written into one SQL constant. A tenth
/// added by a migration and not added there is not a stale row: it is a
/// repo erase that raises a foreign-key violation the first time anyone
/// points across. `pg_constraint` is the only thing that knows the real
/// list, so this asks it.
#[tokio::test]
async fn the_reference_closure_covers_every_non_t_foreign_key_into_memory() {
    let (db_name, pg) = migrated_db().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let found: Vec<(String, String)> = sqlx::query_as(
            "SELECT src.relname::text, a.attname::text
               FROM pg_constraint c
               JOIN pg_class src ON src.oid = c.conrelid
               JOIN pg_class tgt ON tgt.oid = c.confrelid
               CROSS JOIN LATERAL unnest(c.conkey) AS k(attnum)
               JOIN pg_attribute a ON a.attrelid = c.conrelid AND a.attnum = k.attnum
              WHERE c.contype = 'f'
                AND src.relnamespace = 'proxima_code'::regnamespace
                AND tgt.relnamespace = 'proxima_core'::regnamespace
                AND tgt.relname = 'memory'
                AND a.attname <> 't'
                AND c.confdeltype = 'a'
              ORDER BY 1, 2",
        )
        .fetch_all(pg.pool_for_tests())
        .await?;
        let closure = proxima_code::testkit::reference_closure_sql();
        let mut missed = Vec::new();
        for (table, column) in &found {
            let reached = closure.contains(&format!("proxima_code.{table}"))
                && closure.contains(&format!("{column} IN (SELECT t FROM erased)"));
            if !reached {
                missed.push(format!("{table}.{column}"));
            }
        }
        assert!(
            missed.is_empty(),
            "these NO ACTION references into proxima_core.memory are not closed by the \
             repo erase, so a cross-repo pointer through any of them aborts it: {missed:?}"
        );
        assert_eq!(
            found.len(),
            9,
            "the closure was written against nine such columns; the schema now has \
             {} — {found:?}",
            found.len()
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("the_reference_closure_covers_every_non_t_foreign_key_into_memory failed");
}

/// `EraseRule::Cascade { via }` is a claim about the database, and until now
/// nothing checked it.
///
/// The repo-erase completeness test exempts a surface from the sweep on the
/// strength of that declaration alone. Declare `Cascade` and write the
/// foreign key without `ON DELETE CASCADE` and both tests stay green while
/// the rows survive every erase. This asks `pg_constraint` whether the
/// cascade the contract promises is the cascade the schema has.
#[tokio::test]
async fn every_cascade_the_contract_declares_is_a_cascade_the_schema_enforces() {
    let (db_name, pg) = migrated_db().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let mut relations = Vec::new();
        let mut names = Vec::new();
        for surface in proxima_code::contract::CODE_FLAVOR_CONTRACT
            .schemas
            .iter()
            .flat_map(|schema| schema.surfaces.iter())
            .chain(
                proxima_code::contract::CODE_FLAVOR_CONTRACT
                    .state_surfaces
                    .iter(),
            )
        {
            if let proxima_core::flavor::EraseRule::Cascade { via } = surface.erase {
                relations.push(via.relation.to_owned());
                names.push(via.name.to_owned());
            }
        }
        assert!(
            !names.is_empty(),
            "the code flavor declares at least one cascading surface"
        );
        let unenforced: Vec<(String, String)> = sqlx::query_as(
            "SELECT d.relation, d.name
               FROM unnest($1::text[], $2::text[]) AS d(relation, name)
              WHERE NOT EXISTS (
                    SELECT 1
                      FROM pg_constraint c
                      JOIN pg_class src ON src.oid = c.conrelid
                     WHERE c.conname = d.name
                       AND c.contype = 'f'
                       AND c.confdeltype = 'c'
                       AND (src.relnamespace::regnamespace)::text || '.' || src.relname
                           = d.relation
                )",
        )
        .bind(&relations)
        .bind(&names)
        .fetch_all(pg.pool_for_tests())
        .await?;
        assert!(
            unenforced.is_empty(),
            "these surfaces declare EraseRule::Cascade and are exempted from the repo \
             sweep on that declaration, but no ON DELETE CASCADE foreign key of that \
             name backs it: {unenforced:?}"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("every_cascade_the_contract_declares_is_a_cascade_the_schema_enforces failed");
}
