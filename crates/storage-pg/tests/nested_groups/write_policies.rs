use super::{Harness, TestResult, scope};
use proxima_core::GroupId;
use uuid::Uuid;

async fn insert_edge(
    connection: &mut sqlx::PgConnection,
    parent: GroupId,
    child: GroupId,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO proxima_core.group_edges (parent_group_id, child_group_id, relation)
         VALUES ($1, $2, 'viewer')",
    )
    .bind(parent.into_inner())
    .bind(child.into_inner())
    .execute(connection)
    .await?;
    Ok(())
}

async fn assert_unmanaged_edge_refused(parent_managed: bool) -> TestResult {
    let h = Harness::new(None).await?;
    let groups = h.groups(2).await?;
    let managed = if parent_managed { groups[0] } else { groups[1] };
    let mut tx = h.runtime.begin().await?;
    scope(&mut tx, &[managed]).await?;
    let error = insert_edge(&mut tx, groups[0], groups[1])
        .await
        .expect_err("an edge requires manage authority on both groups");
    assert_eq!(
        error
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::code)
            .as_deref(),
        Some("42501"),
        "the runtime role must be refused by RLS: {error}"
    );
    tx.rollback().await?;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM proxima_core.group_edges")
        .fetch_one(&h.pool)
        .await?;
    assert_eq!(count, 0, "refused writes leave no group edge");
    Ok(())
}

#[tokio::test]
async fn group_edge_insert_refuses_unmanaged_child() -> TestResult {
    assert_unmanaged_edge_refused(true).await
}

#[tokio::test]
async fn group_edge_insert_refuses_unmanaged_parent() -> TestResult {
    assert_unmanaged_edge_refused(false).await
}

#[tokio::test]
async fn group_edge_insert_accepts_both_managed_groups() -> TestResult {
    let h = Harness::new(None).await?;
    let groups = h.groups(2).await?;
    let mut tx = h.runtime.begin().await?;
    scope(&mut tx, &groups).await?;
    insert_edge(&mut tx, groups[0], groups[1]).await?;
    tx.commit().await?;
    let edge: (Uuid, Uuid) =
        sqlx::query_as("SELECT parent_group_id, child_group_id FROM proxima_core.group_edges")
            .fetch_one(&h.pool)
            .await?;
    assert_eq!(edge, (groups[0].into_inner(), groups[1].into_inner()));
    Ok(())
}

#[tokio::test]
async fn group_edge_update_requires_both_managed_groups() -> TestResult {
    let h = Harness::new(None).await?;
    let groups = h.groups(2).await?;
    h.add(groups[0], groups[1].into(), proxima_core::Relation::Viewer)
        .await?;
    for managed in &groups {
        let mut tx = h.runtime.begin().await?;
        scope(&mut tx, &[*managed]).await?;
        let error = sqlx::query(
            "UPDATE proxima_core.group_edges SET relation = 'admin' WHERE child_group_id = $1",
        )
        .bind(groups[1].into_inner())
        .execute(&mut *tx)
        .await
        .expect_err("changing a relation requires both groups' manage authority");
        assert_eq!(
            error
                .as_database_error()
                .and_then(sqlx::error::DatabaseError::code)
                .as_deref(),
            Some("42501"),
            "the runtime role must be refused by RLS: {error}"
        );
        tx.rollback().await?;
    }
    let mut tx = h.runtime.begin().await?;
    scope(&mut tx, &groups).await?;
    let updated = sqlx::query(
        "UPDATE proxima_core.group_edges SET relation = 'editor' WHERE child_group_id = $1",
    )
    .bind(groups[1].into_inner())
    .execute(&mut *tx)
    .await?;
    assert_eq!(updated.rows_affected(), 1);
    tx.commit().await?;
    Ok(())
}

#[tokio::test]
async fn group_lock_insert_accepts_managed_member() -> TestResult {
    let h = Harness::new(None).await?;
    let groups = h.groups(2).await?;
    let mut tx = h.runtime.begin().await?;
    scope(&mut tx, &[groups[1]]).await?;
    let (member_is_group,): (bool,) = sqlx::query_as(
        "INSERT INTO proxima_core.group_lock_request
            (request_id, left_group_id, right_group_id)
         VALUES ($1, $2, $3)
         RETURNING member_is_group",
    )
    .bind(Uuid::now_v7())
    .bind(groups[0].into_inner())
    .bind(groups[1].into_inner())
    .fetch_one(&mut *tx)
    .await?;
    assert!(member_is_group, "the managed right endpoint is a group");
    tx.commit().await?;
    Ok(())
}

#[tokio::test]
async fn group_lock_insert_accepts_person_without_member_manage() -> TestResult {
    let h = Harness::new(None).await?;
    let groups = h.groups(1).await?;
    let mut tx = h.runtime.begin().await?;
    scope(&mut tx, &groups).await?;
    sqlx::query(
        "INSERT INTO proxima_core.group_lock_request
            (request_id, left_group_id, right_group_id)
         VALUES ($1, $2, $3)",
    )
    .bind(Uuid::now_v7())
    .bind(groups[0].into_inner())
    .bind(Uuid::now_v7())
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

#[tokio::test]
async fn group_lock_read_hides_unmanaged_endpoints() -> TestResult {
    let h = Harness::new(None).await?;
    let groups = h.groups(3).await?;
    let left_managed = Uuid::now_v7();
    let right_managed = Uuid::now_v7();
    let neither_managed = Uuid::now_v7();
    let mut tx = h.pool.begin().await?;
    sqlx::raw_sql(
        "ALTER TABLE proxima_core.group_lock_request
         DISABLE TRIGGER group_lock_request_trigger;
         ALTER TABLE proxima_core.group_lock_request
         DISABLE TRIGGER group_lock_request_cleanup;",
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.group_lock_request
             (request_id, left_group_id, right_group_id)
         VALUES ($1, $2, $3), ($4, $5, $6), ($7, $8, $9)",
    )
    .bind(left_managed)
    .bind(groups[0].into_inner())
    .bind(groups[1].into_inner())
    .bind(right_managed)
    .bind(groups[1].into_inner())
    .bind(groups[0].into_inner())
    .bind(neither_managed)
    .bind(groups[1].into_inner())
    .bind(groups[2].into_inner())
    .execute(&mut *tx)
    .await?;
    sqlx::raw_sql(
        "ALTER TABLE proxima_core.group_lock_request
         ENABLE TRIGGER group_lock_request_trigger;
         ALTER TABLE proxima_core.group_lock_request
         ENABLE TRIGGER group_lock_request_cleanup;",
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    let mut runtime = h.runtime.begin().await?;
    scope(&mut runtime, &[groups[0]]).await?;
    let visible: Vec<Uuid> = sqlx::query_scalar(
        "SELECT request_id FROM proxima_core.group_lock_request ORDER BY request_id",
    )
    .fetch_all(&mut *runtime)
    .await?;
    let mut expected = vec![left_managed, right_managed];
    expected.sort_unstable();
    assert_eq!(visible, expected);
    runtime.rollback().await?;
    Ok(())
}
