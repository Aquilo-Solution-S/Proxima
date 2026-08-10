use std::sync::Arc;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use futures_util::future::BoxFuture;
use proxima_core::auth::{AuthError, Credentials};
use proxima_core::mcp::{McpTool, McpToolAnnotations, McpToolCtx, McpToolError};
use proxima_core::{
    AccessError, AuthPath, Authenticator, AuthzContext, DelegatedAuthorityError,
    DelegatedAuthorityService, DelegatedCommand, DelegationRevocation, Engine, FlavorRegistry,
    GroupId, OwnerAccessPort, OwnerRef, OwnerRoles, Role, ToolScope, UserId,
};
use proxima_storage_pg::PgDelegationStore;
use uuid::Uuid;

use crate::common::{drop_db, fresh_pg};

const TOOL_NAME: &str = "test-delegation_pg_worker";

struct Worker;

impl McpTool for Worker {
    const NAME: &'static str = TOOL_NAME;
    const DESCRIPTION: &'static str = "PG delegation integration worker";
    const ANNOTATIONS: Option<McpToolAnnotations> =
        Some(McpToolAnnotations::new().read_only(false).open_world(false));
    type Args = std::collections::BTreeMap<String, serde_json::Value>;
    type Output = ();

    fn call(
        _ctx: McpToolCtx,
        _args: std::collections::BTreeMap<String, serde_json::Value>,
    ) -> BoxFuture<'static, Result<(), McpToolError>> {
        Box::pin(async { Ok(()) })
    }
}

#[derive(Debug)]
struct Access {
    subject: UserId,
    owner: OwnerRef,
}

#[async_trait]
impl OwnerAccessPort for Access {
    async fn resolve_roles_for_subject(&self, subject: UserId) -> Result<OwnerRoles, AccessError> {
        assert_eq!(subject, self.subject);
        OwnerRoles::for_subject(subject, [(self.owner, Role::editor())])
    }
}

#[derive(Debug)]
struct Auth;

#[async_trait]
impl Authenticator for Auth {
    async fn authenticate(&self, _creds: &Credentials) -> Result<AuthzContext, AuthError> {
        Err(AuthError::AuthRequired)
    }
}

#[tokio::test]
async fn pg_grants_are_owner_bound_concurrently_revocable_and_retained_for_audit() {
    let (pg, db_name) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let subject = UserId::new(Uuid::now_v7());
        let owner = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
        let foreign_owner = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
        let mut registry = FlavorRegistry::new();
        registry.add_mcp_tool_or_panic_for_tests::<Worker>("test-delegation");
        let (engine, _system, runtime_authority) =
            Engine::new(registry.freeze_or_panic_for_tests()).into_runtime_authorities();
        let registry = Arc::new(engine.registry().clone());
        let command = DelegatedCommand::parse(TOOL_NAME, &registry)?;
        let service = DelegatedAuthorityService::new(
            Arc::new(PgDelegationStore::new(pg.pool_for_tests().clone())),
            Arc::new(Access { subject, owner }),
            Arc::new(Auth),
            registry,
            ToolScope::All,
            &runtime_authority,
        );
        let caller = AuthzContext::for_subject_with_role(
            subject,
            [(owner, Role::editor())],
            AuthPath::HostBearer,
        )
        .with_expires_at(Some(SystemTime::now() + Duration::from_mins(1)))
        .with_tool_scope(ToolScope::Palette(vec![TOOL_NAME.into()]));

        let issued = service
            .issue(&caller, owner, command.clone(), Role::editor())
            .await?;
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM proxima_core.delegated_authority_grants")
                .fetch_one(pg.pool_for_tests())
                .await?;
        assert_eq!(count, 1);

        assert!(matches!(
            service
                .redeem_phase(issued.id, foreign_owner, &command)
                .await,
            Err(DelegatedAuthorityError::NotFound)
        ));
        let _phase = service
            .redeem_phase(issued.id, owner, &command)
            .await
            .expect("exact owner/id grant redeems");

        let first = {
            let service = service.clone();
            let caller = caller.clone();
            tokio::spawn(async move { service.revoke(&caller, issued.id, owner).await })
        };
        let second = {
            let service = service.clone();
            let caller = caller.clone();
            tokio::spawn(async move { service.revoke(&caller, issued.id, owner).await })
        };
        let outcomes = [first.await??, second.await??];
        assert!(outcomes.contains(&DelegationRevocation::Revoked));
        assert!(outcomes.contains(&DelegationRevocation::AlreadyRevoked));
        assert!(matches!(
            service.redeem_phase(issued.id, owner, &command).await,
            Err(DelegatedAuthorityError::Revoked)
        ));

        let retained: (i64, i64) = sqlx::query_as(
            "SELECT count(*), count(*) FILTER (WHERE revoked_at IS NOT NULL)
               FROM proxima_core.delegated_authority_grants",
        )
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(retained, (1, 1));
        Ok(())
    }
    .await;
    let drop_result = drop_db(&db_name).await;
    result.expect("delegated authority PG scenario");
    drop_result.expect("drop delegated authority test database");
}
