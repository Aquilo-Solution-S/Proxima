//! Engine membership authorization for nested group endpoints.

use std::sync::Arc;

use proxima_core::authz::{AuthorizationHook, AuthzInput, AuthzOperation, AuthzVeto};
use proxima_core::test_fixtures::authenticated_context;
use proxima_core::{
    AuthPath, AuthzContext, Engine, FlavorRegistry, GroupId, OwnerId, OwnerMembershipAdminPort,
    Relation, Role, UserId,
};
use proxima_pg_testkit::{create_db, db_url, drop_db, split_role_urls};
use proxima_storage_pg::test_fixtures::register_group_for_tests;
use proxima_storage_pg::{PgPlatformScope, PgStorage};
use proxima_storage_pg::{PgPoolConfig, PgTuning};
use sqlx::PgPool;
use uuid::Uuid;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

#[derive(Debug)]
struct VetoRemovals;

impl AuthorizationHook for VetoRemovals {
    fn veto(&self, input: &AuthzInput<'_>) -> Result<(), AuthzVeto> {
        if matches!(
            input.operation,
            AuthzOperation::Membership {
                change: proxima_core::authz::MembershipChange::Remove,
                ..
            }
        ) {
            Err(AuthzVeto("member removal vetoed".into()))
        } else {
            Ok(())
        }
    }
}

struct Harness {
    name: String,
    storage: Arc<PgStorage>,
    engine: Engine,
    parent: GroupId,
    child: GroupId,
    admin: UserId,
}

impl Harness {
    async fn new() -> TestResult<Self> {
        Self::new_with_hook(None).await
    }

    async fn new_with_hook(hook: Option<Arc<dyn AuthorizationHook>>) -> TestResult<Self> {
        let name = format!("proxima_test_nested_engine_{}", Uuid::now_v7().simple());
        create_db(&name).await?;
        let admin_pool = PgPool::connect(&db_url(&name)).await?;
        let (runtime_url, platform_url) = split_role_urls(&name).await?;
        let migration = proxima_storage_pg::PgMigrationStorage::connect_with_config(
            &platform_url,
            PgPoolConfig::default(),
            PgTuning::default(),
        )
        .await?;
        migration.run_migrations().await?;
        migration.initialize_store_number(1).await?;
        let platform_pool = PgPool::connect(&platform_url).await?;
        let platform_scope = PgPlatformScope::new(platform_pool.clone(), &["proxima_core"]).await?;
        let storage = Arc::new(
            PgStorage::connect_with_config(
                &runtime_url,
                PgPoolConfig::default(),
                PgTuning::default(),
                platform_scope,
                1,
            )
            .await?,
        );
        let parent = GroupId::new(Uuid::now_v7());
        let child = GroupId::new(Uuid::now_v7());
        register_group_for_tests(&admin_pool, parent).await?;
        register_group_for_tests(&admin_pool, child).await?;
        let admin = UserId::new(Uuid::now_v7());
        storage
            .bootstrap_group_admin(parent, admin, admin.into_inner())
            .await?;
        storage
            .bootstrap_group_admin(child, admin, admin.into_inner())
            .await?;
        let mut registry = FlavorRegistry::new();
        if let Some(hook) = hook {
            registry.add_authorization_hook(hook);
        }
        let engine = Engine::new(registry.freeze_or_panic_for_tests())
            .with_storage_ports(storage.clone().storage_ports());
        Ok(Self {
            name,
            storage,
            engine,
            parent,
            child,
            admin,
        })
    }

    async fn finish(self) {
        drop(self.engine);
        drop(self.storage);
        let _ = drop_db(&self.name).await;
    }
}

#[tokio::test]
async fn engine_requires_manage_on_both_endpoints_to_attach_a_group() -> TestResult {
    let h = Harness::new().await?;
    let parent_only = authenticated_context(AuthzContext::for_subject_with_role(
        h.admin,
        [(OwnerId::from(h.parent), Role::admin())],
        AuthPath::HostBearer,
    ));
    let error = h
        .engine
        .add_member(
            &parent_only,
            h.parent,
            OwnerId::from(h.child),
            Relation::Viewer,
        )
        .await
        .expect_err("attached-group manage is required");
    assert_eq!(error.code, proxima_core::ErrorCode::Forbidden);

    let child_only = authenticated_context(AuthzContext::for_subject_with_role(
        h.admin,
        [(OwnerId::from(h.child), Role::admin())],
        AuthPath::HostBearer,
    ));
    let error = h
        .engine
        .add_member(
            &child_only,
            h.parent,
            OwnerId::from(h.child),
            Relation::Viewer,
        )
        .await
        .expect_err("parent manage is required");
    assert_eq!(error.code, proxima_core::ErrorCode::Forbidden);
    h.finish().await;
    Ok(())
}

#[tokio::test]
async fn engine_allows_group_detach_from_either_endpoint() -> TestResult {
    let h = Harness::new().await?;
    let both = authenticated_context(AuthzContext::for_subject_with_role(
        h.admin,
        [
            (OwnerId::from(h.parent), Role::admin()),
            (OwnerId::from(h.child), Role::admin()),
        ],
        AuthPath::HostBearer,
    ));
    h.engine
        .add_member(&both, h.parent, OwnerId::from(h.child), Relation::Viewer)
        .await?;

    let child_only = authenticated_context(AuthzContext::for_subject_with_role(
        h.admin,
        [(OwnerId::from(h.child), Role::admin())],
        AuthPath::HostBearer,
    ));
    h.engine
        .remove_member(&child_only, h.parent, OwnerId::from(h.child))
        .await?;

    h.engine
        .add_member(&both, h.parent, OwnerId::from(h.child), Relation::Viewer)
        .await?;
    h.engine
        .remove_member_relation(
            &child_only,
            h.parent,
            OwnerId::from(h.child),
            Relation::Viewer,
        )
        .await?;
    h.engine
        .add_member(&both, h.parent, OwnerId::from(h.child), Relation::Viewer)
        .await?;
    let parent_only = authenticated_context(AuthzContext::for_subject_with_role(
        h.admin,
        [(OwnerId::from(h.parent), Role::admin())],
        AuthPath::HostBearer,
    ));
    h.engine
        .remove_member(&parent_only, h.parent, OwnerId::from(h.child))
        .await?;
    h.finish().await;
    Ok(())
}

#[tokio::test]
async fn member_endpoint_detach_removes_edge_without_parent_read_authority() -> TestResult {
    let h = Harness::new().await?;
    let both = authenticated_context(AuthzContext::for_subject_with_role(
        h.admin,
        [
            (OwnerId::from(h.parent), Role::admin()),
            (OwnerId::from(h.child), Role::admin()),
        ],
        AuthPath::HostBearer,
    ));
    let child_only = authenticated_context(AuthzContext::for_subject_with_role(
        h.admin,
        [(OwnerId::from(h.child), Role::admin())],
        AuthPath::HostBearer,
    ));
    assert!(!child_only.may_read(&h.parent.into(), proxima_core::AccessKind::Fact));
    let admin_pool = PgPool::connect(&db_url(&h.name)).await?;
    for relation in [None, Some(Relation::Viewer)] {
        h.engine
            .add_member(&both, h.parent, OwnerId::from(h.child), Relation::Viewer)
            .await?;
        match relation {
            None => {
                h.engine
                    .remove_member(&child_only, h.parent, h.child.into())
                    .await?;
            }
            Some(relation) => {
                h.engine
                    .remove_member_relation(&child_only, h.parent, h.child.into(), relation)
                    .await?;
            }
        }
        let remaining: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM proxima_core.group_edges WHERE child_group_id = $1",
        )
        .bind(h.child.into_inner())
        .fetch_one(&admin_pool)
        .await?;
        assert_eq!(remaining, 0, "the member admin actually removes the edge");
    }
    admin_pool.close().await;
    h.finish().await;
    Ok(())
}

#[tokio::test]
async fn member_endpoint_detach_runs_the_remove_hook() -> TestResult {
    let h = Harness::new_with_hook(Some(Arc::new(VetoRemovals))).await?;
    let both = authenticated_context(AuthzContext::for_subject_with_role(
        h.admin,
        [
            (OwnerId::from(h.parent), Role::admin()),
            (OwnerId::from(h.child), Role::admin()),
        ],
        AuthPath::HostBearer,
    ));
    h.engine
        .add_member(&both, h.parent, OwnerId::from(h.child), Relation::Viewer)
        .await?;
    let child_only = authenticated_context(AuthzContext::for_subject_with_role(
        h.admin,
        [(OwnerId::from(h.child), Role::admin())],
        AuthPath::HostBearer,
    ));
    let error = h
        .engine
        .remove_member(&child_only, h.parent, OwnerId::from(h.child))
        .await
        .expect_err("the removal hook must run before a member-endpoint detach");
    assert_eq!(error.code, proxima_core::ErrorCode::Forbidden);
    h.finish().await;
    Ok(())
}

#[tokio::test]
async fn member_endpoint_cannot_replace_a_group_relation() -> TestResult {
    let h = Harness::new().await?;
    let both = authenticated_context(AuthzContext::for_subject_with_role(
        h.admin,
        [
            (OwnerId::from(h.parent), Role::admin()),
            (OwnerId::from(h.child), Role::admin()),
        ],
        AuthPath::HostBearer,
    ));
    h.engine
        .add_member(&both, h.parent, OwnerId::from(h.child), Relation::Viewer)
        .await?;
    let child_only = authenticated_context(AuthzContext::for_subject_with_role(
        h.admin,
        [(OwnerId::from(h.child), Role::admin())],
        AuthPath::HostBearer,
    ));
    let error = h
        .engine
        .replace_member_relation(
            &child_only,
            h.parent,
            OwnerId::from(h.child),
            Relation::Viewer,
            Relation::Editor,
        )
        .await
        .expect_err("relation replacement requires parent manage authority");
    assert_eq!(error.code, proxima_core::ErrorCode::Forbidden);
    h.finish().await;
    Ok(())
}
