//! The per-owner half of the Postgres access port.
//!
//! `PgOwnerAccessResolver::resolve_role_for_owner` answers out of one probe
//! on the membership primary key, not the member's whole enumeration. What is
//! pinned here is that the probe is *byte-identical* to the eager map's answer
//! for every owner shape — a mapped group, an unmapped group, a group with
//! more than one relation, the subject's own personal owner, a stranger's —
//! so a host that serves many parties through one forwarder subject can lean
//! on it without a second authorization vocabulary.

use proxima_core::{GroupId, OwnerAccessPort, OwnerRef, Relation, Role, UserId};
use proxima_pg_testkit::{create_db, db_url, drop_db};
use proxima_storage_pg::{PgOwnerAccessResolver, PgStorage};
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

async fn seed_membership(pool: &sqlx::PgPool, group: GroupId, member: UserId, relation: Relation) {
    sqlx::query(
        "INSERT INTO proxima_core.group_memberships (group_id, member_user_id, relation)
         VALUES ($1, $2, $3)",
    )
    .bind(group.into_inner())
    .bind(member.into_inner())
    .bind(relation)
    .execute(pool)
    .await
    .expect("seed membership");
}

#[tokio::test]
async fn per_owner_probe_matches_the_eager_map_for_every_owner_shape() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(error) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {error}");
    }
    let result = async {
        let pg = PgStorage::connect(&db_url(&db_name)).await?;
        pg.run_migrations().await?;
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect(&db_url(&db_name))
            .await?;

        let forwarder = UserId::new(Uuid::now_v7());
        let stranger = UserId::new(Uuid::now_v7());
        let editor_group = GroupId::new(Uuid::now_v7());
        let layered_group = GroupId::new(Uuid::now_v7());
        let unmapped_group = GroupId::new(Uuid::now_v7());
        seed_membership(&pool, editor_group, forwarder, Relation::Editor).await;
        seed_membership(&pool, layered_group, forwarder, Relation::Viewer).await;
        seed_membership(&pool, layered_group, forwarder, Relation::Ingest).await;
        // Someone else's membership in the unmapped group must not leak in.
        seed_membership(&pool, unmapped_group, stranger, Relation::Admin).await;

        let port = PgOwnerAccessResolver::new(pool);
        let eager = port.resolve_roles_for_subject(forwarder).await?;

        for owner in [
            OwnerRef::Group(editor_group),
            OwnerRef::Group(layered_group),
            OwnerRef::Group(unmapped_group),
            OwnerRef::Personal(forwarder),
            OwnerRef::Personal(stranger),
        ] {
            let probed = port.resolve_role_for_owner(forwarder, owner).await?;
            assert_eq!(
                probed,
                eager.role_for(&owner),
                "per-owner probe must equal the eager map for {owner:?}"
            );
        }

        assert_eq!(
            port.resolve_role_for_owner(forwarder, OwnerRef::Group(editor_group))
                .await?,
            Some(Role::editor())
        );
        assert_eq!(
            port.resolve_role_for_owner(forwarder, OwnerRef::Group(unmapped_group))
                .await?,
            None,
            "a group the subject holds no relation in is no role, never a default one"
        );
        assert_eq!(
            port.resolve_role_for_owner(forwarder, OwnerRef::Personal(forwarder))
                .await?,
            Some(Role::personal())
        );
        assert_eq!(
            port.resolve_role_for_owner(forwarder, OwnerRef::Personal(stranger))
                .await?,
            None
        );
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("per-owner probe matches the eager map");
}
