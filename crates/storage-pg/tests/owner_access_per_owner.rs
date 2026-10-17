//! The per-group half of the Postgres access port.
//!
//! `PgOwnerAccessResolver::resolve_group_role` answers out of one probe on
//! the membership primary key, not the member's whole enumeration. What is
//! pinned here is that the probe is *byte-identical* to the eager map's
//! answer for every group shape — a mapped group, an unmapped group, a group
//! with more than one relation (resolved to their join). The separate owner
//! registry probe recognises both Personal and Group owners and refuses
//! unregistered ids, using the resolver's validated platform authority.

use proxima_core::{GroupId, OwnerAccessPort, OwnerId, Relation, Role, UserId};
use proxima_pg_testkit::{db_url, drop_db};
use proxima_storage_pg::test_fixtures::create_core_db;
use proxima_storage_pg::{PgOwnerAccessResolver, PgStorage};
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

async fn seed_membership(pool: &sqlx::PgPool, group: GroupId, member: UserId, relation: Relation) {
    sqlx::query(
        "WITH registered AS (
              INSERT INTO proxima_core.owners (owner_id, kind) VALUES ($1, 'group')
              ON CONFLICT (owner_id) DO NOTHING
          )
          INSERT INTO proxima_core.group_memberships (group_id, member_user_id, relation)
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
async fn per_group_probe_matches_the_eager_map_for_every_group_shape() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(error) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {error}");
    }
    let result = async {
        let pg = PgStorage::connect_for_tests(db_url(&db_name)).await?;
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect(&db_url(&db_name))
            .await?;

        let forwarder = UserId::new(Uuid::now_v7());
        let stranger = UserId::new(Uuid::now_v7());
        let editor_group = GroupId::new(Uuid::now_v7());
        let layered_group = GroupId::new(Uuid::now_v7());
        let unmapped_group = GroupId::new(Uuid::now_v7());
        let promoted_group = GroupId::new(Uuid::now_v7());
        let personal = OwnerId::from(UserId::new(Uuid::now_v7()));
        let unknown = OwnerId::new(Uuid::now_v7());
        sqlx::query(
            "INSERT INTO proxima_core.owners (owner_id, kind)
             VALUES ($1, 'personal')",
        )
        .bind(personal.into_inner())
        .execute(&pool)
        .await?;
        seed_membership(&pool, editor_group, forwarder, Relation::Editor).await;
        // Promoted after joining: an Admin row beside the Viewer row. The rows
        // come back in enum order, Admin first.
        seed_membership(&pool, promoted_group, forwarder, Relation::Viewer).await;
        seed_membership(&pool, promoted_group, forwarder, Relation::Admin).await;
        seed_membership(&pool, layered_group, forwarder, Relation::Viewer).await;
        seed_membership(&pool, layered_group, forwarder, Relation::Ingest).await;
        // Someone else's membership in the unmapped group must not leak in.
        seed_membership(&pool, unmapped_group, stranger, Relation::Admin).await;

        let port = PgOwnerAccessResolver::new(pool, pg.platform_scope_for_host());
        assert!(port.is_registered_owner(personal).await?);
        assert!(
            port.is_registered_owner(OwnerId::from(editor_group))
                .await?
        );
        assert!(
            port.is_registered_owner(OwnerId::from(unmapped_group))
                .await?
        );
        assert!(
            !port.is_registered_owner(unknown).await?,
            "an unknown id never becomes registered through a probe"
        );
        let eager = port.resolve_roles_for_subject(forwarder).await?;

        for group in [editor_group, layered_group, promoted_group, unmapped_group] {
            let probed = port.resolve_group_role(forwarder, group).await?;
            assert_eq!(
                probed,
                eager.role_for(&OwnerId::from(group)),
                "per-group probe must equal the eager map for {group:?}"
            );
        }

        assert_eq!(
            port.resolve_group_role(forwarder, editor_group).await?,
            Some(Role::editor())
        );
        // Several relations in one group resolve to their join, never to the
        // last (weakest) row.
        assert_eq!(
            port.resolve_group_role(forwarder, promoted_group).await?,
            Some(Role::admin())
        );
        assert_eq!(
            port.resolve_group_role(forwarder, layered_group).await?,
            Some(Role::viewer().join(Role::ingest()))
        );
        assert_eq!(
            port.resolve_group_role(forwarder, unmapped_group).await?,
            None,
            "a group the subject holds no relation in is no role, never a default one"
        );
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("per-group probe matches the eager map");
}
