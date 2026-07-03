use std::sync::Arc;
use std::time::Duration;

use crate::common::{drop_db, fresh_pg, owner_fixture, owner_write_permit};
use proxima_core::storage_ports::SourceCursorPort;
use proxima_core::{
    AccessKind, AuthPath, AuthzContext, Cursor, Engine, ErrorCode, FlavorRegistry, GroupId, Owner,
    OwnerRef, Role, UserId,
};

fn engine_for(pg: proxima_storage_pg::PgStorage) -> Engine {
    Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests())
        .with_storage_ports(Arc::new(pg).storage_ports())
}

async fn cursor_updated_at(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
    source: &str,
) -> Result<time::OffsetDateTime, sqlx::Error> {
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(owner);
    sqlx::query_scalar(
        "SELECT updated_at
           FROM proxima_core.source_cursors
          WHERE owner_kind = $1
            AND owner_id = $2
            AND source = $3",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .bind(source)
    .fetch_one(pg.pool_for_tests())
    .await
}

#[tokio::test]
async fn source_cursor_round_trips_opaque_bytes() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let owner = owner_fixture();
        let permit = owner_write_permit(&owner, AccessKind::Fact).await?;
        let bytes = vec![0x00, 0xff, 0xfe, b'A', 0x80, 0x00, 0x7f];
        let cursor = Cursor::from_bytes(bytes.clone());

        pg.store_source_cursor(&permit, "evidence/projector", &cursor)
            .await?;
        let loaded = pg
            .load_source_cursor(&owner, "evidence/projector")
            .await?
            .expect("stored cursor exists");

        assert_eq!(loaded.as_bytes(), bytes.as_slice());
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn source_cursor_upsert_replaces_bytes_and_advances_timestamp()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let owner = owner_fixture();
        let permit = owner_write_permit(&owner, AccessKind::Fact).await?;
        let source = "evidence/projector";
        let first = Cursor::from_bytes(vec![0xff, 0x00, 0x01]);
        let second = Cursor::from_bytes(vec![0x02, 0xfe, 0xfd, 0x00]);

        pg.store_source_cursor(&permit, source, &first).await?;
        let first_updated_at = cursor_updated_at(&pg, &owner, source).await?;
        tokio::time::sleep(Duration::from_millis(10)).await;

        pg.store_source_cursor(&permit, source, &second).await?;
        let loaded = pg
            .load_source_cursor(&owner, source)
            .await?
            .expect("upserted cursor exists");
        let second_updated_at = cursor_updated_at(&pg, &owner, source).await?;

        assert_eq!(loaded.as_bytes(), second.as_bytes());
        assert!(
            second_updated_at > first_updated_at,
            "upsert must advance updated_at"
        );
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn source_cursor_absent_owner_source_returns_none() -> Result<(), Box<dyn std::error::Error>>
{
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let owner = owner_fixture();
        let missing = pg.load_source_cursor(&owner, "missing/source").await?;
        assert_eq!(missing, None);
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn source_cursor_storage_keys_by_owner_and_engine_denies_cross_owner()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let owner_a = owner_fixture();
        let owner_b = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
        let permit_a = owner_write_permit(&owner_a, AccessKind::Fact).await?;
        let permit_b = owner_write_permit(&owner_b, AccessKind::Fact).await?;
        let source = "evidence/projector";
        let cursor_b = Cursor::from_bytes(vec![0xff, 0x10, 0x00]);
        let cursor_a = Cursor::from_bytes(vec![0x01, 0x02, 0x03]);

        pg.store_source_cursor(&permit_b, source, &cursor_b).await?;
        assert_eq!(
            pg.load_source_cursor(&owner_a, source).await?,
            None,
            "owner A must not read owner B's row through storage owner scoping"
        );

        pg.store_source_cursor(&permit_a, source, &cursor_a).await?;
        assert_eq!(
            pg.load_source_cursor(&owner_b, source)
                .await?
                .expect("owner B row remains")
                .as_bytes(),
            cursor_b.as_bytes(),
            "owner A storage upsert must not overwrite owner B's row"
        );

        let engine = engine_for(pg.clone());
        let authz_a = AuthzContext::single_owner(&owner_a, AuthPath::HostBearer);
        let engine_cursor_a = Cursor::from_bytes(vec![0x7f, 0x00, 0x80]);
        engine
            .store_source_cursor(&authz_a, &owner_a, source, &engine_cursor_a)
            .await?;
        assert_eq!(
            engine
                .load_source_cursor(&authz_a, &owner_a, source)
                .await?
                .expect("authorized owner A engine read succeeds")
                .as_bytes(),
            engine_cursor_a.as_bytes()
        );
        assert_eq!(
            pg.load_source_cursor(&owner_b, source)
                .await?
                .expect("owner B row remains after owner A engine write")
                .as_bytes(),
            cursor_b.as_bytes()
        );

        let read_err = engine
            .load_source_cursor(&authz_a, &owner_b, source)
            .await
            .expect_err("owner A authz cannot read owner B cursor state");
        assert_eq!(read_err.code, ErrorCode::Forbidden);

        let write_err = engine
            .store_source_cursor(&authz_a, &owner_b, source, &Cursor::from_bytes(vec![0x99]))
            .await
            .expect_err("owner A authz cannot overwrite owner B cursor state");
        assert_eq!(write_err.code, ErrorCode::Forbidden);

        assert_eq!(
            pg.load_source_cursor(&owner_b, source)
                .await?
                .expect("owner B row remains after denied engine write")
                .as_bytes(),
            cursor_b.as_bytes()
        );
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn source_cursor_age_is_read_authorized_without_cursor_body_access()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let owner = OwnerRef::Group(GroupId::new(uuid::Uuid::now_v7()));
        let viewer = UserId::new(uuid::Uuid::now_v7());
        let permit = owner_write_permit(&owner, AccessKind::Fact).await?;
        let source = "evidence/projector";

        pg.store_source_cursor(&permit, source, &Cursor::from_bytes(vec![0x01, 0x02]))
            .await?;
        tokio::time::sleep(Duration::from_millis(10)).await;

        let engine = engine_for(pg.clone());
        let viewer_authz = AuthzContext::for_subject_with_role(
            viewer,
            [(owner, Role::viewer())],
            AuthPath::HostBearer,
        );
        let age = engine
            .source_cursor_age(&viewer_authz, &owner, source)
            .await?
            .expect("stored cursor has age");
        assert!(age <= Duration::from_mins(1), "cursor age should be recent");

        let load_err = engine
            .load_source_cursor(&viewer_authz, &owner, source)
            .await
            .expect_err("viewer must not load opaque cursor bytes");
        assert_eq!(load_err.code, ErrorCode::Forbidden);

        let write_err = engine
            .store_source_cursor(
                &viewer_authz,
                &owner,
                source,
                &Cursor::from_bytes(vec![0x03]),
            )
            .await
            .expect_err("viewer must not mutate cursor bytes");
        assert_eq!(write_err.code, ErrorCode::Forbidden);

        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}
