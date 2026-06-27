use crate::common;

#[tokio::test]
async fn migration_creates_entity_owner_and_membership() {
    let (pg, db) = common::fresh_pg().await;
    let pool = pg.pool();

    let (n,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM information_schema.tables
          WHERE table_schema='proxima_core'
            AND table_name IN ('entity_owner','group_membership')",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(n, 2, "both access tables exist");

    let eid = uuid::Uuid::now_v7();
    let ins = |kind: &str, home: bool| {
        sqlx::query(
            "INSERT INTO proxima_core.entity_owner
                (entity_id, owner_principal_kind, owner_principal_id, is_home, granted_by)
             VALUES ($1,$2::proxima_core.owner_principal_kind,$3,$4,$5)",
        )
        .bind(eid)
        .bind(kind.to_string())
        .bind(uuid::Uuid::now_v7())
        .bind(home)
        .bind(uuid::Uuid::now_v7())
    };
    ins("User", true).execute(pool).await.unwrap();
    let dup = ins("Group", true).execute(pool).await;
    assert!(
        dup.is_err(),
        "second home row must violate uq_entity_owner_home"
    );

    common::drop_db(&db).await.unwrap();
}
