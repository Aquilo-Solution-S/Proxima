//! The `retention` rows through the Engine: set by whoever may erase that
//! owner's memory by hand, read back for exactly the owners the caller
//! administers, and read whole under the host's system authority.

#[path = "fixtures/example_engine.rs"]
#[allow(dead_code, reason = "each suite uses part of the shared fixture")]
mod example_engine;

use example_engine::ExampleEngine;
use proxiima_core::{
    AuthPath, AuthzContext, ErrorCode, KeepDays, OwnerId, Retention, Role, SchemaId, UserId,
};
use uuid::Uuid;

const MAIL: &str = "proxiima-example/note-v1";
const PAGE: &str = "proxiima-example/digest-v1";

fn days(days: i64) -> KeepDays {
    KeepDays::new(days).expect("in range")
}

fn schema(id: &str) -> SchemaId {
    SchemaId::new(id.to_owned())
}

/// A registered person, and the context it signs in with, holding `roles`
/// on other owners.
async fn person(
    h: &ExampleEngine,
    roles: &[(OwnerId, Role)],
) -> Result<(OwnerId, AuthzContext), sqlx::Error> {
    let user = UserId::new(Uuid::now_v7());
    register(h, OwnerId::from(user), "personal").await?;
    let authz = proxiima_core::test_fixtures::authenticated_context(
        AuthzContext::for_subject_with_role(user, roles.iter().copied(), AuthPath::HostBearer),
    );
    Ok((OwnerId::from(user), authz))
}

async fn register(h: &ExampleEngine, owner: OwnerId, kind: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO proxiima_core.owners (owner_id, kind)
         VALUES ($1, $2::proxiima_core.owner_kind) ON CONFLICT DO NOTHING",
    )
    .bind(owner.into_inner())
    .bind(kind)
    .execute(&h.pool)
    .await?;
    Ok(())
}

fn row(owner: OwnerId, schema_id: &str, keep: i64) -> Retention {
    Retention {
        owner,
        schema: schema(schema_id),
        keep_days: days(keep),
    }
}

/// Rows in the order the read answers: by owner, then schema.
fn sorted(mut rows: Vec<Retention>) -> Vec<Retention> {
    rows.sort_by(|a, b| (a.owner, a.schema.as_str()).cmp(&(b.owner, b.schema.as_str())));
    rows
}

#[tokio::test]
async fn a_row_is_set_by_whoever_may_erase_that_owners_memory() {
    let h = ExampleEngine::new("proxiima_retention", |_| {}).await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let group = OwnerId::new(Uuid::now_v7());
        register(&h, group, "group").await?;
        let (admin, admin_authz) = person(&h, &[(group, Role::admin())]).await?;
        let (editor, editor_authz) = person(&h, &[(group, Role::editor())]).await?;
        let (_viewer, viewer_authz) = person(&h, &[(group, Role::viewer())]).await?;
        let (_ingest, ingest_authz) = person(&h, &[(group, Role::ingest())]).await?;
        let (outsider, outsider_authz) = person(&h, &[]).await?;

        h.engine
            .set_retention(&admin_authz, &group, &schema(MAIL), Some(days(30)))
            .await?;
        for (who, authz) in [
            ("editor", &editor_authz),
            ("viewer", &viewer_authz),
            ("ingest", &ingest_authz),
            ("outsider", &outsider_authz),
        ] {
            let refused = h
                .engine
                .set_retention(authz, &group, &schema(MAIL), Some(days(1)))
                .await
                .expect_err("only the group's admin sets its row");
            assert_eq!(refused.code, ErrorCode::Forbidden, "{who}");
        }
        h.engine
            .set_retention(&admin_authz, &admin, &schema(PAGE), Some(days(7)))
            .await?;
        h.engine
            .set_retention(&editor_authz, &editor, &schema(MAIL), Some(days(90)))
            .await?;
        h.engine
            .set_retention(&outsider_authz, &outsider, &schema(PAGE), Some(days(365)))
            .await?;

        assert_eq!(
            h.engine.read_retention(&admin_authz).await?,
            sorted(vec![row(group, MAIL, 30), row(admin, PAGE, 7)]),
            "the admin reads its own rows and its group's"
        );
        assert_eq!(
            h.engine.read_retention(&editor_authz).await?,
            vec![row(editor, MAIL, 90)],
            "an editor reads its own rows, not the group's"
        );
        assert_eq!(
            h.engine.read_retention(&viewer_authz).await?,
            Vec::new(),
            "a viewer with no rows of its own reads none"
        );

        h.engine
            .set_retention(&admin_authz, &group, &schema(MAIL), Some(days(60)))
            .await?;
        h.engine
            .set_retention(&admin_authz, &admin, &schema(PAGE), None)
            .await?;
        assert_eq!(
            h.engine.read_retention(&admin_authz).await?,
            vec![row(group, MAIL, 60)],
            "a set replaces the row, and a set with no days removes it"
        );

        let (engine, system) = h.engine.into_system_authority();
        assert_eq!(
            engine.all_retention(&system).await?,
            sorted(vec![
                row(group, MAIL, 60),
                row(editor, MAIL, 90),
                row(outsider, PAGE, 365),
            ]),
            "the host's clock reads every owner's rows"
        );
        let (_, foreign) = proxiima_core::Engine::new(
            proxiima_core::SpiiceRegistry::new().freeze_or_panic_for_tests(),
        )
        .into_system_authority();
        let refused = engine
            .all_retention(&foreign)
            .await
            .expect_err("another boot's authority reads nothing");
        assert_eq!(refused.code, ErrorCode::Forbidden);
        Ok(())
    }
    .await;
    result.expect("retention authority");
}
