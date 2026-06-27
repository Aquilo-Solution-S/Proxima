use crate::common;
use proxima_core::{EntityId, GroupId, MemoryId, Principal, Relation, Storage, UserId};

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

#[tokio::test]
async fn entity_is_readable_respects_membership() {
    let (pg, db) = common::fresh_pg().await;
    let p = Principal::User(UserId::new(uuid::Uuid::now_v7()));
    let q = Principal::User(UserId::new(uuid::Uuid::now_v7()));
    let g1 = GroupId::new(uuid::Uuid::now_v7());

    seed_membership(&pg, g1, &p, Relation::Viewer).await;
    seed_membership(&pg, g1, &q, Relation::Viewer).await;
    let f1 = seed_memory_owned(&pg, Principal::Group(g1)).await;
    let a = seed_memory_owned(&pg, p.clone()).await;
    let s_p = read_owners(&pg, &p).await;
    let s_q = read_owners(&pg, &q).await;

    assert!(pg.entity_is_readable(f1, &s_p).await.unwrap());
    assert!(pg.entity_is_readable(a, &s_p).await.unwrap());
    assert!(pg.entity_is_readable(f1, &s_q).await.unwrap());
    assert!(
        !pg.entity_is_readable(a, &s_q).await.unwrap(),
        "A is personal to P"
    );

    common::drop_db(&db).await.unwrap();
}

async fn seed_membership(
    pg: &proxima_storage_pg::PgStorage,
    group: GroupId,
    member: &Principal,
    relation: Relation,
) {
    let Principal::User(user) = member else {
        panic!("seed_membership only accepts user members");
    };

    sqlx::query(
        "INSERT INTO proxima_core.group_membership
            (group_id, member_user_id, relation, granted_by)
         VALUES ($1,$2,$3::proxima_core.membership_relation,$4)",
    )
    .bind(group.into_inner())
    .bind(user.into_inner())
    .bind(relation)
    .bind(uuid::Uuid::now_v7())
    .execute(pg.pool())
    .await
    .unwrap();
}

async fn seed_memory_owned(pg: &proxima_storage_pg::PgStorage, owner: Principal) -> EntityId {
    let entity_id = uuid::Uuid::now_v7();
    let (owner_kind, owner_id) = owner.columns();
    sqlx::query(
        "INSERT INTO proxima_core.entity_owner
            (entity_id, owner_principal_kind, owner_principal_id, is_home, granted_by)
         VALUES ($1,$2::proxima_core.owner_principal_kind,$3,true,$4)",
    )
    .bind(entity_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(uuid::Uuid::now_v7())
    .execute(pg.pool())
    .await
    .unwrap();
    EntityId::Memory(MemoryId::new(entity_id))
}

async fn read_owners(pg: &proxima_storage_pg::PgStorage, principal: &Principal) -> Vec<Principal> {
    let mut owners = vec![principal.clone()];
    owners.extend(
        pg.resolve_membership(principal)
            .await
            .unwrap()
            .into_iter()
            .map(|membership| Principal::Group(membership.group)),
    );
    owners.push(proxima_core::access::world());
    owners
}
