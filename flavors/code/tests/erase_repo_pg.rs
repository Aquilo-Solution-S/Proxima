mod common;

use common::{migrated_db, test_owner};
use proxima_code::CodeFlavorStore;
use proxima_code::CommitV1;
use proxima_code::RepoScope;
use proxima_code::testkit::{erase_footprint, erase_repo, register_repo};
use proxima_core::{FactPayload, Owner};
use proxima_pg_testkit::{db_url, drop_db};
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
    insert_series(pool, owner, t, &[]).await.map(|_| ())
}

/// A second version of an existing series: same handle, new `t`, head
/// moved.
///
/// The ordinary write path produces these on every supersede, and nothing
/// constrains a later version to name the same repository as the first —
/// which is why an erase footprint that stops at the versions its own rows
/// named is a footprint with a hole in it.
async fn insert_next_version(
    pool: &sqlx::PgPool,
    owner: &Owner,
    handle: Uuid,
    t: Uuid,
    sidecars: &[&str],
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO proxima_core.memory
            (handle, t, kind, owner_id, schema_id, sidecar_tables)
         VALUES ($1, $2, 'fact', $3, $4, $5)",
    )
    .bind(handle)
    .bind(t)
    .bind(owner.stored_owner_id())
    .bind(CommitV1::SCHEMA_ID)
    .bind(stamp(sidecars))
    .execute(pool)
    .await?;
    sqlx::query("UPDATE proxima_core.memory_head SET t = $2 WHERE handle = $1")
        .bind(handle)
        .bind(t)
        .execute(pool)
        .await?;
    Ok(())
}

/// `memory.sidecar_tables` as the write path stamps it.
///
/// Not decoration: it is how [`erase_memory`] knows which flavor rows
/// belong to an admission, and it is the ONLY path to a sidecar row whose
/// `repo_id` is not the repo being erased — the flavor sweep filters on
/// `repo_id = $1` and never sees it. A fixture that leaves it empty is a
/// fixture no write path produces.
fn stamp(sidecars: &[&str]) -> Vec<String> {
    sidecars.iter().map(|table| (*table).to_owned()).collect()
}

/// [`insert_memory`], returning the series handle it minted.
async fn insert_series(
    pool: &sqlx::PgPool,
    owner: &Owner,
    t: Uuid,
    sidecars: &[&str],
) -> Result<Uuid, sqlx::Error> {
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
        "INSERT INTO proxima_core.memory
            (handle, t, kind, owner_id, schema_id, sidecar_tables)
         VALUES ($1, $2, 'fact', $3, $4, $5)",
    )
    .bind(handle)
    .bind(t)
    .bind(owner_id)
    .bind(CommitV1::SCHEMA_ID)
    .bind(stamp(sidecars))
    .execute(pool)
    .await?;
    Ok(handle)
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
/// observations", which makes a perspective SERIES that never named this
/// repository the owner's, not any one repository's — the same standing
/// `engineer_self_v1` has. It goes with the owner, through the compliance
/// erase.
///
/// The series is the unit, so the sibling test
/// `a_perspective_that_dropped_its_repo_id_still_goes_with_the_repo` pins
/// the other half: a NULL `repo_id` on a version whose series WAS filed
/// here is not a way out.
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
        let admission: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.memory WHERE t = $1")
                .bind(cross_repo)
                .fetch_one(pool)
                .await?;
        assert_eq!(
            admission, 1,
            "a surviving sidecar row whose admission went is a row nothing can read; \
             the substrate half has to survive too"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("a_perspective_about_no_particular_repo_survives_a_repo_erase failed");
}

/// Every `(table, column)` a statement tests with
/// `column IN (SELECT t FROM erased)`.
///
/// Pairs, not two independent `contains` checks. `work_item_memory_id`
/// names four different tables, so asking "is the table mentioned" and "is
/// the column mentioned" separately passes for a table that is named for
/// some other column entirely, and passes for three of the nine references
/// on the strength of the fourth.
fn erased_pairs_of(sql: &str) -> std::collections::BTreeSet<(String, String)> {
    let mut table = String::new();
    let mut pairs = std::collections::BTreeSet::new();
    for line in sql.lines() {
        let line = line.trim();
        if let Some(named) = line
            .split(|c: char| c.is_whitespace() || matches!(c, '(' | ')' | ',' | '\''))
            .find(|token| token.starts_with("proxima_code."))
        {
            named.clone_into(&mut table);
        }
        if let Some(rest) = line.strip_suffix(" IN (SELECT t FROM erased)")
            && let Some(column) = rest.rsplit(' ').next()
        {
            pairs.insert((table.clone(), column.to_owned()));
        }
    }
    pairs
}

/// The closure statements' column lists, asked of the database.
///
/// The list is nine `(table, column)` pairs hand-written into two SQL
/// constants — the one that finds the references and the one that deletes
/// them. A tenth added by a migration and not added there is not a stale
/// row: it is a repo erase that raises a foreign-key violation the first
/// time anyone points across. `pg_constraint` is the only thing that knows
/// the real list, so this asks it.
///
/// `RESTRICT` is asked for as well as `NO ACTION`. They differ only in when
/// the check runs, never in whether it fires, so a tenth reference written
/// as `RESTRICT` would break every repo erase exactly as a `NO ACTION` one
/// does — while a filter on `confdeltype = 'a'` alone would keep counting
/// nine and stay green.
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
                AND c.confdeltype IN ('a', 'r')
              ORDER BY 1, 2",
        )
        .fetch_all(pg.pool_for_tests())
        .await?;
        let mut missed = Vec::new();
        for sql in proxima_code::testkit::reference_closure_sql() {
            let pairs = erased_pairs_of(sql);
            for (table, column) in &found {
                if !pairs.contains(&(format!("proxima_code.{table}"), column.clone())) {
                    missed.push(format!("{table}.{column}"));
                }
            }
        }
        missed.sort_unstable();
        missed.dedup();
        assert!(
            missed.is_empty(),
            "these references into proxima_core.memory are not closed by the repo erase, \
             so a cross-repo pointer through any of them aborts it: {missed:?}"
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
///
/// Through `all_surfaces()`, not a hand-rolled union of `schemas` and
/// `state_surfaces` — the same blindness the sibling completeness test had
/// deleted from it. The hand-rolled union cannot see the projection
/// surface, and the projection is `EraseRule::Cascade` AND exempted from
/// the repo sweep on exactly that declaration, so it was the one surface
/// whose cascade most needed asking about and the one this could not ask.
#[tokio::test]
async fn every_cascade_the_contract_declares_is_a_cascade_the_schema_enforces() {
    let (db_name, pg) = migrated_db().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let mut relations = Vec::new();
        let mut names = Vec::new();
        for surface in proxima_code::contract::CODE_FLAVOR_CONTRACT.all_surfaces() {
            if let proxima_core::flavor::EraseRule::Cascade { via } = surface.erase {
                relations.push(via.relation.to_owned());
                names.push(via.name.to_owned());
            }
        }
        assert!(
            !names.is_empty(),
            "the code flavor declares at least one cascading surface"
        );
        assert!(
            relations
                .iter()
                .any(|relation| relation == "proxima_code.projection"),
            "the projection is Cascade-declared and exempt from the repo sweep on that \
             declaration alone, so it is the one surface this test may not be blind to; \
             found {relations:?}"
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

/// The version the sweep never saw.
///
/// Reproduces the abort that survived the first repo-erase fix. A work item
/// is superseded — the ordinary write path, `derive_append` reuses the
/// handle — and the new version is filed under a different repository,
/// which nothing forbids. Erasing the first repository sweeps v1 only; the
/// substrate then erases the whole series, v2 included, and v2's
/// `work_assignment_v1` is still pointing at it:
/// `work_assignment_v1_work_item_memory_id_fkey`, and the whole erase rolls
/// back.
///
/// The footprint has to be a joint fixpoint of "what this repo's rows name"
/// and "what the series expansion adds" for this to pass, which is the
/// whole point.
#[tokio::test]
async fn a_superseded_version_filed_elsewhere_is_part_of_the_footprint() {
    let (db_name, pg) = migrated_db().await;
    let result = exercise_superseded_version_erase(pg.pool_for_tests()).await;
    let _ = drop_db(&db_name).await;
    result.expect("a_superseded_version_filed_elsewhere_is_part_of_the_footprint failed");
}

/// A work item superseded into a second repository, with an assignment
/// naming only the second version. Returns `(owner, erased_repo, ids)`.
async fn seed_superseded_series(
    pool: &sqlx::PgPool,
) -> Result<(Owner, Uuid, [Uuid; 4]), Box<dyn std::error::Error>> {
    let owner = test_owner();
    let erased_repo = Uuid::now_v7();
    let other_repo = Uuid::now_v7();
    for (repo_id, label) in [(erased_repo, "erased"), (other_repo, "other")] {
        register_repo(
            pool,
            &owner,
            repo_id,
            &format!("/tmp/proxima-erase-series-{repo_id}"),
            label,
            &RepoScope::default(),
        )
        .await?;
    }

    // v1 under the repo about to go.
    let v1 = Uuid::now_v7();
    let handle = insert_series(pool, &owner, v1, &["proxima_code.work_requested_v1"]).await?;
    sqlx::query(
        "INSERT INTO proxima_code.work_requested_v1
            (t, repo_id, title, instructions, request_key)
         VALUES ($1, $2, 'v1', 'instructions', 'req-series')",
    )
    .bind(v1)
    .bind(erased_repo)
    .execute(pool)
    .await?;

    // v2 of the SAME series, filed under the other repo.
    let v2 = Uuid::now_v7();
    insert_next_version(
        pool,
        &owner,
        handle,
        v2,
        &["proxima_code.work_requested_v1"],
    )
    .await?;
    sqlx::query(
        "INSERT INTO proxima_code.work_requested_v1
            (t, repo_id, title, instructions, request_key)
         VALUES ($1, $2, 'v2', 'instructions', 'req-series-2')",
    )
    .bind(v2)
    .bind(other_repo)
    .execute(pool)
    .await?;

    // ... and a row in the other repo pointing at v2, not v1.
    let engineer = Uuid::now_v7();
    insert_memory(pool, &owner, engineer).await?;
    sqlx::query(
        "INSERT INTO proxima_code.engineer_self_v1 (t, display_name, purpose)
         VALUES ($1, 'engineer', 'writes code')",
    )
    .bind(engineer)
    .execute(pool)
    .await?;
    let assignment = Uuid::now_v7();
    insert_memory(pool, &owner, assignment).await?;
    sqlx::query(
        "INSERT INTO proxima_code.work_assignment_v1
            (t, repo_id, work_item_memory_id, target_perspective_memory_id, reason)
         VALUES ($1, $2, $3, $4, 'assigned against the new version')",
    )
    .bind(assignment)
    .bind(other_repo)
    .bind(v2)
    .bind(engineer)
    .execute(pool)
    .await?;

    Ok((owner, erased_repo, [v1, v2, engineer, assignment]))
}

async fn exercise_superseded_version_erase(
    pool: &sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    {
        let (owner, erased_repo, [v1, v2, engineer, assignment]) =
            seed_superseded_series(pool).await?;
        let store = CodeFlavorStore::from_backend_pool_for_tests(pool.clone());
        let receipt = erase_repo(&store, &owner, erased_repo).await?;
        assert!(receipt.repo_record_deleted);
        assert_eq!(
            receipt.memories_deleted, 3,
            "both versions of the series and the assignment that named the second"
        );

        let left: Vec<String> = sqlx::query_scalar(
            "SELECT 'work_requested_v1 v1' AS relation
               FROM proxima_code.work_requested_v1 WHERE t = $1
             UNION ALL
             SELECT 'work_requested_v1 v2'
               FROM proxima_code.work_requested_v1 WHERE t = $2
             UNION ALL
             SELECT 'work_assignment_v1'
               FROM proxima_code.work_assignment_v1 WHERE t = $3
             ORDER BY relation",
        )
        .bind(v1)
        .bind(v2)
        .bind(assignment)
        .fetch_all(pool)
        .await?;
        assert!(
            left.is_empty(),
            "a series is erased whole, and what points into it goes with it: {left:?}"
        );
        let engineer_left: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_code.engineer_self_v1 WHERE t = $1",
        )
        .bind(engineer)
        .fetch_one(pool)
        .await?;
        assert_eq!(
            engineer_left, 1,
            "the assignment's TARGET is the owner's self-model and outlives the repo"
        );
        Ok(())
    }
}

/// The other half of the nullable `repo_id` reading.
///
/// A perspective first filed under this repository, then superseded by a
/// version that says `repo_id: None`. The version says "no particular
/// repo"; the SERIES was this repository's, and a series is erased whole —
/// keeping v2 while v1 goes would leave a head pointing at a row that no
/// longer exists. So it goes, and the doc on the sweep says so.
#[tokio::test]
async fn a_perspective_that_dropped_its_repo_id_still_goes_with_the_repo() {
    let (db_name, pg) = migrated_db().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pool = pg.pool_for_tests();
        let owner = test_owner();
        let repo_id = Uuid::now_v7();
        let _ = seed_work_item_fixture(pool, &owner, repo_id).await?;

        let v1 = Uuid::now_v7();
        let handle = insert_series(
            pool,
            &owner,
            v1,
            &["proxima_code.development_perspective_v1"],
        )
        .await?;
        sqlx::query(
            "INSERT INTO proxima_code.development_perspective_v1
                (t, repo_id, summary, pattern, risk, recommended_posture, confidence)
             VALUES ($1, $2, 'about this repo', 'p', 'r', 'rp', 0.5)",
        )
        .bind(v1)
        .bind(repo_id)
        .execute(pool)
        .await?;

        let v2 = Uuid::now_v7();
        insert_next_version(
            pool,
            &owner,
            handle,
            v2,
            &["proxima_code.development_perspective_v1"],
        )
        .await?;
        sqlx::query(
            "INSERT INTO proxima_code.development_perspective_v1
                (t, repo_id, summary, pattern, risk, recommended_posture, confidence)
             VALUES ($1, NULL, 'about the codebase at large now', 'p', 'r', 'rp', 0.5)",
        )
        .bind(v2)
        .execute(pool)
        .await?;

        let store = CodeFlavorStore::from_backend_pool_for_tests(pool.clone());
        erase_repo(&store, &owner, repo_id).await?;

        let left: Vec<String> = sqlx::query_scalar(
            "SELECT 'development_perspective_v1 v2' AS relation
               FROM proxima_code.development_perspective_v1 WHERE t = $1
             UNION ALL
             SELECT 'its admission' FROM proxima_core.memory WHERE t = $1
             UNION ALL
             SELECT 'its head' FROM proxima_core.memory_head WHERE handle = $2
             ORDER BY relation",
        )
        .bind(v2)
        .bind(handle)
        .fetch_all(pool)
        .await?;
        assert!(
            left.is_empty(),
            "a version that dropped its repo_id is still a version of this repo's \
             series, and a series is erased whole: {left:?}"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("a_perspective_that_dropped_its_repo_id_still_goes_with_the_repo failed");
}

/// A repo erase is one owner's authority, and it stops at that boundary.
///
/// A code sidecar's foreign key names `proxima_core.memory (t)` and nothing
/// in it constrains whose memory that is, so a second principal's row can
/// point into this repo — after a transfer, most ordinarily. Deleting it
/// would destroy that principal's data on this principal's say-so, and
/// silently: their memory row survives, its sidecar does not, and nothing
/// anywhere says why. Refusing and naming the rows is the only answer that
/// leaves someone able to act.
#[tokio::test]
async fn a_reference_from_another_owner_stops_the_erase_and_names_it() {
    let (db_name, pg) = migrated_db().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pool = pg.pool_for_tests();
        let owner = test_owner();
        let stranger = test_owner();
        let erased_repo = Uuid::now_v7();
        let WorkItemFixture { work_item, .. } =
            seed_work_item_fixture(pool, &owner, erased_repo).await?;

        let stranger_repo = Uuid::now_v7();
        register_repo(
            pool,
            &stranger,
            stranger_repo,
            &format!("/tmp/proxima-erase-stranger-{stranger_repo}"),
            "another principal's repo",
            &RepoScope::default(),
        )
        .await?;
        let stray = Uuid::now_v7();
        insert_memory(pool, &stranger, stray).await?;
        sqlx::query(
            "INSERT INTO proxima_code.execution_result_v1
                (t, repo_id, work_requested_memory_id, status, summary, artifact_refs)
             VALUES ($1, $2, $3, 'succeeded', 'not yours to erase', ARRAY[]::text[])",
        )
        .bind(stray)
        .bind(stranger_repo)
        .bind(work_item)
        .execute(pool)
        .await?;

        let store = CodeFlavorStore::from_backend_pool_for_tests(pool.clone());
        let err = erase_repo(&store, &owner, erased_repo)
            .await
            .expect_err("another principal's reference must stop the erase");
        let message = err.to_string();
        assert!(
            message.contains("proxima_code.execution_result_v1")
                && message.contains(&stray.to_string()),
            "the refusal has to name the rows the operator must act on: {message}"
        );

        let intact: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_code.work_requested_v1 WHERE t = $1",
        )
        .bind(work_item)
        .fetch_one(pool)
        .await?;
        assert_eq!(intact, 1, "a refused erase erases nothing");
        let stranger_row: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_code.execution_result_v1 WHERE t = $1",
        )
        .bind(stray)
        .fetch_one(pool)
        .await?;
        assert_eq!(stranger_row, 1, "and least of all another principal's rows");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("a_reference_from_another_owner_stops_the_erase_and_names_it failed");
}

/// The one write that would make the erase abort, attempted while the erase
/// holds its locks.
///
/// `lock_timeout` and not `statement_timeout`: both stop the test hanging,
/// but only `lock_timeout` distinguishes the two outcomes this has to tell
/// apart. `57014` says "the statement ran too long", which a slow machine
/// produces just as readily as a held lock; `55P03` says "gave up WAITING
/// FOR A LOCK", which is the claim being made. `SET LOCAL` so it dies with
/// the transaction rather than riding a pooled connection into another
/// test.
async fn insert_assignment_with_timeout(
    pool: &sqlx::PgPool,
    t: Uuid,
    repo_id: Uuid,
    work_item: Uuid,
    engineer: Uuid,
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("SET LOCAL lock_timeout = '2500ms'")
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT INTO proxima_code.work_assignment_v1
            (t, repo_id, work_item_memory_id, target_perspective_memory_id, reason)
         VALUES ($1, $2, $3, $4, 'concurrent')",
    )
    .bind(t)
    .bind(repo_id)
    .bind(work_item)
    .bind(engineer)
    .execute(&mut *tx)
    .await?;
    tx.commit().await
}

fn sqlstate_of(err: &sqlx::Error) -> Option<String> {
    match err {
        sqlx::Error::Database(db) => db.code().map(|code| code.to_string()),
        _ => None,
    }
}

/// The lock is load-bearing, and this is what says so.
///
/// Everything else about the erase passes with `lock_admissions_for_erase`
/// replaced by nothing: the deletes are correct, the footprint is correct,
/// and the only thing missing is the guarantee that no row referencing the
/// footprint can be committed between computing it and deleting it. That
/// guarantee is only observable from a second session, so this opens one:
/// the same INSERT lands immediately when the footprint is not held, gives
/// up WAITING FOR A LOCK when it is, and lands again once the erase rolls
/// back.
///
/// `FOR UPDATE` on the referenced admission is what does it. Inserting a
/// row with a foreign key takes `FOR KEY SHARE` on the row it references,
/// and `FOR KEY SHARE` conflicts with `FOR UPDATE` and with nothing weaker
/// — so the writer waits, and then fails its own foreign key in its own
/// transaction instead of aborting the erase in ours.
#[tokio::test]
async fn the_footprint_is_locked_against_a_concurrent_reference() {
    let (db_name, pg) = migrated_db().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pool = pg.pool_for_tests();
        let owner = test_owner();
        let repo_id = Uuid::now_v7();
        let WorkItemFixture {
            work_item,
            engineer,
            ..
        } = seed_work_item_fixture(pool, &owner, repo_id).await?;

        let writer = sqlx::PgPool::connect(&db_url(&db_name)).await?;
        let unblocked = Uuid::now_v7();
        insert_memory(pool, &owner, unblocked).await?;
        let blocked = Uuid::now_v7();
        insert_memory(pool, &owner, blocked).await?;

        // Nothing held: the write lands.
        insert_assignment_with_timeout(&writer, unblocked, repo_id, work_item, engineer)
            .await
            .expect("with no erase in flight the reference is an ordinary write");

        let mut tx = pool.begin().await?;
        let footprint = erase_footprint(&mut tx, &owner, repo_id).await?;
        assert!(
            footprint.contains(&work_item),
            "the work item is in the footprint the erase just locked"
        );

        let err = insert_assignment_with_timeout(&writer, blocked, repo_id, work_item, engineer)
            .await
            .expect_err("a reference into a locked footprint must wait, not land");
        assert_eq!(
            sqlstate_of(&err).as_deref(),
            Some("55P03"),
            "the write should have given up WAITING FOR THE ERASE'S ROW LOCK — 55P03, \
             not a statement that merely ran long; instead it failed with {err}"
        );

        // And once the erase lets go, the same write is ordinary again: the
        // lock was the whole reason, not anything about the row.
        tx.rollback().await?;
        insert_assignment_with_timeout(&writer, blocked, repo_id, work_item, engineer)
            .await
            .expect("once the erase has rolled back the reference lands");
        writer.close().await;
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("the_footprint_is_locked_against_a_concurrent_reference failed");
}

/// The reported deadlock, reproduced, and the reason the erase survives it.
///
/// An erase holding a row lock on the lower admission and asking for the
/// higher one, against a writer holding `FOR KEY SHARE` on the higher and
/// waiting for the lower: a cycle, and no lock ordering on the erase's side
/// prevents it, because the writer's order is its own. `PostgreSQL` breaks
/// the cycle by aborting whichever transaction closed it.
///
/// Computing the whole footprint first and locking it in ONE statement
/// shrinks the erase's half of this window to the inside of that statement,
/// which is why the interleaving below has to be staged by hand rather than
/// driven through `erase_repo`. What makes the erase survive the residue is
/// that `40P01` is classified as transient and the whole transaction is
/// re-run — so this pins the classification against a deadlock this
/// database really raised, not against a table of SQLSTATEs.
#[tokio::test]
async fn a_deadlock_against_a_concurrent_writer_is_classified_as_retryable() {
    let (db_name, pg) = migrated_db().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pool = pg.pool_for_tests();
        let owner = test_owner();
        let repo_id = Uuid::now_v7();
        let WorkItemFixture { engineer, .. } =
            seed_work_item_fixture(pool, &owner, repo_id).await?;

        let (mut lo, mut hi) = (Uuid::now_v7(), Uuid::now_v7());
        if lo > hi {
            std::mem::swap(&mut lo, &mut hi);
        }
        for t in [lo, hi] {
            insert_memory(pool, &owner, t).await?;
        }
        let (first, second) = (Uuid::now_v7(), Uuid::now_v7());
        for t in [first, second] {
            insert_memory(pool, &owner, t).await?;
        }

        // The erase, mid-lock: it holds the lower admission.
        let mut erase = pool.begin().await?;
        sqlx::query("SELECT t FROM proxima_core.memory WHERE t = $1 FOR UPDATE")
            .bind(lo)
            .fetch_all(&mut *erase)
            .await?;

        // The writer: holds a key share on the higher admission, then asks
        // for one on the lower and waits.
        let writer_pool = sqlx::PgPool::connect(&db_url(&db_name)).await?;
        let writer = tokio::spawn(async move {
            let mut tx = writer_pool.begin().await?;
            for (t, reference) in [(first, hi), (second, lo)] {
                sqlx::query(
                    "INSERT INTO proxima_code.work_assignment_v1
                        (t, repo_id, work_item_memory_id, target_perspective_memory_id, reason)
                     VALUES ($1, $2, $3, $4, 'concurrent')",
                )
                .bind(t)
                .bind(repo_id)
                .bind(reference)
                .bind(engineer)
                .execute(&mut *tx)
                .await?;
            }
            tx.commit().await
        });

        // Wait until the writer is genuinely blocked, so the request below
        // closes a cycle rather than racing one.
        let mut waiting = false;
        for _ in 0..200 {
            waiting = sqlx::query_scalar::<_, i64>(
                // A row-lock wait registers as a `transactionid` lock,
                // whose `pg_locks.database` is NULL — so this asks
                // `pg_stat_activity`, which is scoped by `datname` and
                // says plainly that a backend is waiting on a lock.
                "SELECT count(*)::bigint
                   FROM pg_stat_activity
                  WHERE datname = current_database()
                    AND wait_event_type = 'Lock'",
            )
            .fetch_one(pool)
            .await?
                > 0;
            if waiting {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        assert!(
            waiting,
            "the writer never blocked; the interleaving did not set up"
        );

        sqlx::query("SET LOCAL statement_timeout = '15s'")
            .execute(&mut *erase)
            .await?;
        let erase_result = sqlx::query("SELECT t FROM proxima_core.memory WHERE t = $1 FOR UPDATE")
            .bind(hi)
            .fetch_all(&mut *erase)
            .await;

        let ((Err(victim), _) | (Ok(_), Err(victim))) = (erase_result, writer.await?) else {
            panic!("the cycle resolved without anyone being aborted")
        };
        assert_eq!(
            sqlstate_of(&victim).as_deref(),
            Some("40P01"),
            "this interleaving is a deadlock; got {victim}"
        );
        assert!(
            proxima_storage_pg::is_transient_conflict(&victim),
            "a deadlock is what the erase retries on — if this is not classified as \
             transient, the erase surfaces it to the caller instead: {victim}"
        );
        let _ = erase.rollback().await;
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("a_deadlock_against_a_concurrent_writer_is_classified_as_retryable failed");
}

/// The seed leg of the same boundary, reached the way production reaches
/// it.
///
/// The sibling test above builds the cross-owner row by hand and reaches it
/// by FOLLOWING a reference. This one changes nothing by hand: it registers
/// a repo, writes a work item into it, and hands that memory to another
/// principal with the shipped transfer verb. The transfer moves
/// `memory.owner_id` and nothing else — `repo_id` is a flavor column and
/// appears nowhere in the owner-column machinery — so the row keeps the
/// source's `repo_id` while the admission belongs to the destination, and
/// the repo sweep's `repo_id = $1` finds it on the SEED leg, before any
/// reference is followed.
///
/// That leg was unguarded: the ownership question was asked only of rows
/// reached in step 3. The erase deleted the destination's sidecar row on
/// the source's authority and left their `memory` row stamping a table it
/// was no longer in, with no error of any kind — the row was in the
/// footprint, so even `FootprintIncomplete` stayed quiet.
#[tokio::test]
async fn a_transferred_admission_stops_the_erase_instead_of_being_swept() {
    let (db_name, pg) = migrated_db().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pool = pg.pool_for_tests();
        let owner = test_owner();
        let repo_id = Uuid::now_v7();
        let WorkItemFixture { work_item, .. } =
            seed_work_item_fixture(pool, &owner, repo_id).await?;

        // The real verb, not a hand-written UPDATE.
        let stranger = proxima_core::OwnerRef::Group(proxima_core::GroupId::new(Uuid::now_v7()));
        sqlx::query(
            "INSERT INTO proxima_core.owners (owner_id, kind)
             VALUES ($1, 'group') ON CONFLICT DO NOTHING",
        )
        .bind(stranger.stored_owner_id())
        .execute(pool)
        .await?;
        let permit = common::owner_write_permit(&owner, proxima_core::AccessKind::Fact).await?;
        let moved = proxima_core::storage_ports::OwnerTransferPort::transfer_to_owner(
            &pg,
            &permit,
            proxima_core::EntityId::Memory(proxima_core::MemoryId::new(work_item)),
            stranger,
        )
        .await?;
        assert!(moved, "the transfer verb must move the work item");

        let still_here: Option<Uuid> =
            sqlx::query_scalar("SELECT repo_id FROM proxima_code.work_requested_v1 WHERE t = $1")
                .bind(work_item)
                .fetch_optional(pool)
                .await?;
        assert_eq!(
            still_here,
            Some(repo_id),
            "the transfer moves owner_id and nothing else, so the sidecar row keeps \
             the repo it was written into — which is what puts it on the sweep's seed leg"
        );

        let store = CodeFlavorStore::from_backend_pool_for_tests(pool.clone());
        let err = erase_repo(&store, &owner, repo_id)
            .await
            .expect_err("a transferred admission is not this owner's to erase");
        let message = err.to_string();
        assert!(
            message.contains("proxima_code.work_requested_v1")
                && message.contains(&work_item.to_string()),
            "the refusal has to name the row on the seed leg too: {message}"
        );

        let survivors: Vec<String> = sqlx::query_scalar(
            "SELECT 'work_requested_v1' AS relation
               FROM proxima_code.work_requested_v1 WHERE t = $1
             UNION ALL
             SELECT 'its admission' FROM proxima_core.memory WHERE t = $1
             ORDER BY relation",
        )
        .bind(work_item)
        .fetch_all(pool)
        .await?;
        assert_eq!(
            survivors.len(),
            2,
            "a refused erase leaves the other principal's row and its admission \
             both intact: {survivors:?}"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("a_transferred_admission_stops_the_erase_instead_of_being_swept failed");
}

/// Waiting for a lock is bounded, and giving up is retried.
///
/// Nothing on the erase path used to set `lock_timeout`, so the wait was
/// bounded only by the pool's five-minute `statement_timeout` — and what it
/// ended in, `57014`, is not a transient code, so no retry recognised it.
/// A single long-lived `FOR KEY SHARE` holder therefore cost five minutes
/// of blocking and then a hard failure. `SET LOCAL lock_timeout` turns the
/// same situation into `55P03` in five seconds, which IS transient, so the
/// attempt rolls back — releasing everything it held — and comes round
/// again.
///
/// The holder here takes `acceptance_criteria_v1`, which carries no
/// `repo_id`: that leaves the repo row free, so the erase gets past its
/// first lock and blocks on the one this is about — the footprint's.
#[tokio::test]
async fn a_lock_the_erase_cannot_get_is_bounded_and_retried_not_waited_out() {
    let (db_name, pg) = migrated_db().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pool = pg.pool_for_tests();
        let owner = test_owner();
        let repo_id = Uuid::now_v7();
        let WorkItemFixture { work_item, .. } =
            seed_work_item_fixture(pool, &owner, repo_id).await?;

        let holder_pool = sqlx::PgPool::connect(&db_url(&db_name)).await?;
        let latecomer = Uuid::now_v7();
        insert_memory(pool, &owner, latecomer).await?;
        let mut holder = holder_pool.begin().await?;
        sqlx::query(
            "INSERT INTO proxima_code.acceptance_criteria_v1
                (t, work_item_memory_id, criteria_count)
             VALUES ($1, $2, 1)",
        )
        .bind(latecomer)
        .bind(work_item)
        .execute(&mut *holder)
        .await?;

        let store = CodeFlavorStore::from_backend_pool_for_tests(pool.clone());
        let started = std::time::Instant::now();
        let err = erase_repo(&store, &owner, repo_id)
            .await
            .expect_err("a footprint this erase cannot lock is not a footprint it may delete");
        let waited = started.elapsed();
        assert!(
            waited < std::time::Duration::from_mins(2),
            "the erase waited {waited:?} — with no lock_timeout it waits out the pool's \
             statement_timeout instead of giving up and retrying"
        );
        let message = err.to_string();
        assert!(
            message.contains("lock timeout"),
            "giving up on a lock is 55P03 and says so; got {err}"
        );

        // Released: the same erase is ordinary again, which is what makes
        // the give-up worth retrying rather than surfacing.
        holder.rollback().await?;
        erase_repo(&store, &owner, repo_id)
            .await
            .expect("once the holder is gone the erase takes the lock and completes");
        holder_pool.close().await;
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("a_lock_the_erase_cannot_get_is_bounded_and_retried_not_waited_out failed");
}
