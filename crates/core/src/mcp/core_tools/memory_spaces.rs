use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::mcp::{McpTool, McpToolCtx, McpToolError};
use crate::{MemoryActionSet, MemorySpaceGrants, Owner};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MemorySpacesArgs {}

#[derive(Debug, Serialize)]
pub struct MemorySpacesOutput {
    pub spaces: Vec<MemorySpaceOutput>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MemorySpaceOutput {
    pub key: String,
    pub label: String,
    pub actions: MemoryActionSetOutput,
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "wire projection mirrors the five independent memory-space actions"
)]
#[derive(Debug, Clone, Serialize)]
pub struct MemoryActionSetOutput {
    pub search: bool,
    pub read: bool,
    pub write: bool,
    pub publish: bool,
    pub admin: bool,
}

impl From<MemoryActionSet> for MemoryActionSetOutput {
    fn from(actions: MemoryActionSet) -> Self {
        Self {
            search: actions.search,
            read: actions.read,
            write: actions.write,
            publish: actions.publish,
            admin: actions.admin,
        }
    }
}

#[derive(Debug)]
pub struct MemorySpacesTool;

impl McpTool for MemorySpacesTool {
    const NAME: &'static str = "core_memory_spaces";
    const DESCRIPTION: &'static str = "List memory spaces this caller may use. Space keys are selectors only; every use is re-authorized.";
    type Args = MemorySpacesArgs;
    type Output = MemorySpacesOutput;

    fn call(
        ctx: McpToolCtx,
        _args: MemorySpacesArgs,
    ) -> futures::future::BoxFuture<'static, Result<MemorySpacesOutput, McpToolError>> {
        Box::pin(async move {
            Ok(MemorySpacesOutput {
                spaces: list_memory_spaces(&ctx),
            })
        })
    }
}

#[must_use]
pub fn list_memory_spaces(ctx: &McpToolCtx) -> Vec<MemorySpaceOutput> {
    match &ctx.authz.capabilities.memory_spaces {
        MemorySpaceGrants::LegacyAccessiblePrincipals => vec![MemorySpaceOutput {
            key: "current".into(),
            label: "Current owner".into(),
            actions: MemoryActionSet::legacy_from_roles(ctx.authz.capabilities.roles).into(),
        }],
        MemorySpaceGrants::Explicit(grants) => grants
            .iter()
            .filter(|grant| ctx.authz.identity.can_access_principal(&grant.owner))
            .map(|grant| MemorySpaceOutput {
                key: grant.key.clone(),
                label: grant.label.clone(),
                actions: effective_actions(grant.actions, ctx.authz.capabilities.roles).into(),
            })
            .collect(),
    }
}

fn effective_actions(actions: MemoryActionSet, roles: crate::RoleSet) -> MemoryActionSet {
    MemoryActionSet {
        search: actions.search && roles.graph_read,
        read: actions.read && roles.graph_read,
        write: actions.write && roles.graph_write,
        publish: actions.publish && roles.graph_write,
        admin: actions.admin && roles.admin,
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedMemorySpace {
    pub key: String,
    pub label: String,
    pub owner: Owner,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpaceDefault {
    Current,
    Identity,
}

/// Resolve a public space key to a server-issued Owner. The key is a selector
/// only: callers must still check the returned Owner with the concrete verb's
/// role gate plus `AuthzContext::allows_memory_grant`.
///
/// # Errors
///
/// Returns `InvalidInput` when a provided key is unknown, or when omitted
/// default resolution has no explicit grant matching the default Owner.
pub fn resolve_space_owner(
    ctx: &McpToolCtx,
    raw: Option<&str>,
    default: SpaceDefault,
) -> Result<ResolvedMemorySpace, McpToolError> {
    let default_owner = match default {
        SpaceDefault::Current => ctx.owner.clone(),
        SpaceDefault::Identity => ctx.authz.identity.principal.clone(),
    };
    match &ctx.authz.capabilities.memory_spaces {
        MemorySpaceGrants::LegacyAccessiblePrincipals => {
            if let Some(key) = raw
                && key != "current"
            {
                return Err(McpToolError::InvalidInput(format!(
                    "unknown memory space: {key}"
                )));
            }
            Ok(ResolvedMemorySpace {
                key: "current".into(),
                label: "Current owner".into(),
                owner: default_owner,
            })
        }
        MemorySpaceGrants::Explicit(grants) => {
            let grant = match raw {
                Some(key) => grants.iter().find(|grant| grant.key == key),
                None => grants.iter().find(|grant| grant.owner == default_owner),
            }
            .filter(|grant| ctx.authz.identity.can_access_principal(&grant.owner));
            grant
                .map(|grant| ResolvedMemorySpace {
                    key: grant.key.clone(),
                    label: grant.label.clone(),
                    owner: grant.owner.clone(),
                })
                .ok_or_else(|| {
                    McpToolError::InvalidInput(raw.map_or_else(
                        || "current memory space is not granted".to_string(),
                        |key| format!("unknown memory space: {key}"),
                    ))
                })
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::Arc;

    use crate::mcp::{McpAuthorContext, McpTool, McpToolExtensions, OutputMode};
    use crate::{
        AuthPath, AuthzContext, CapabilitySet, FlavorRegistry, GroupId, Identity, MemoryActionSet,
        MemorySpaceGrant, MemorySpaceGrants, Principal, RoleSet, ToolScope, UserId,
    };

    use super::*;

    fn make_ctx_with_spaces(grants: Vec<MemorySpaceGrant>) -> McpToolCtx {
        let principal = grants.first().map_or_else(
            || Principal::User(UserId::new(uuid::Uuid::now_v7())),
            |grant| grant.owner.clone(),
        );
        let accessible_principals = grants
            .iter()
            .map(|grant| grant.owner.clone())
            .collect::<HashSet<_>>();
        make_ctx(
            principal.clone(),
            principal,
            accessible_principals,
            MemorySpaceGrants::explicit(grants),
        )
    }

    fn make_ctx(
        owner: Principal,
        principal: Principal,
        accessible_principals: HashSet<Principal>,
        memory_spaces: MemorySpaceGrants,
    ) -> McpToolCtx {
        McpToolCtx {
            owner,
            authz: AuthzContext {
                identity: Identity {
                    principal,
                    accessible_principals,
                    expires_at: None,
                    auth_epoch: 0,
                },
                capabilities: CapabilitySet {
                    tool_scope: ToolScope::All,
                    roles: RoleSet::all(),
                    memory_spaces,
                },
                auth_path: AuthPath::HostBearer,
            },
            handles: None,
            mode: OutputMode::PrefixedIds,
            registry: Arc::new(FlavorRegistry::new().freeze()),
            author: McpAuthorContext {
                model_id: "test".into(),
                client_name: "test".into(),
                client_version: "0".into(),
                personality_instance_id: None,
                caller_self_perspective: None,
            },
            caller_self_perspective: None,
            master_token_id: None,
            extensions: McpToolExtensions::default(),
            engine: None,
        }
    }

    #[tokio::test]
    async fn memory_spaces_lists_explicit_grants_without_raw_authority() {
        let personal = Principal::User(UserId::new(uuid::Uuid::now_v7()));
        let shared = Principal::Group(GroupId::new(uuid::Uuid::now_v7()));
        let ctx = make_ctx_with_spaces(vec![
            MemorySpaceGrant {
                key: "personal".into(),
                label: "Personal".into(),
                owner: personal,
                actions: MemoryActionSet::read_write_publish_admin(),
            },
            MemorySpaceGrant {
                key: "shared".into(),
                label: "Shared".into(),
                owner: shared,
                actions: MemoryActionSet::read_only(),
            },
        ]);

        let out = MemorySpacesTool::call(ctx, MemorySpacesArgs {})
            .await
            .unwrap();
        assert_eq!(out.spaces.len(), 2);
        assert_eq!(out.spaces[0].key, "personal");
        assert!(out.spaces[0].actions.write);
        assert_eq!(out.spaces[1].key, "shared");
        assert!(!out.spaces[1].actions.write);
    }

    #[test]
    fn explicit_resolution_omitted_space_matches_current_owner_grant() {
        let personal = Principal::User(UserId::new(uuid::Uuid::now_v7()));
        let shared = Principal::Group(GroupId::new(uuid::Uuid::now_v7()));
        let ctx = make_ctx_with_spaces(vec![
            MemorySpaceGrant {
                key: "personal".into(),
                label: "Personal".into(),
                owner: personal.clone(),
                actions: MemoryActionSet::all(),
            },
            MemorySpaceGrant {
                key: "shared".into(),
                label: "Shared".into(),
                owner: shared,
                actions: MemoryActionSet::read_only(),
            },
        ]);

        let resolved = resolve_space_owner(&ctx, None, SpaceDefault::Current).unwrap();
        assert_eq!(resolved.key, "personal");
        assert_eq!(resolved.owner, personal);
    }

    #[test]
    fn explicit_resolution_rejects_grant_without_owner_visibility() {
        let user = Principal::User(UserId::new(uuid::Uuid::now_v7()));
        let hidden = Principal::Group(GroupId::new(uuid::Uuid::now_v7()));
        let mut accessible_principals = HashSet::new();
        accessible_principals.insert(user.clone());
        let ctx = make_ctx(
            user.clone(),
            user,
            accessible_principals,
            MemorySpaceGrants::explicit(vec![MemorySpaceGrant {
                key: "hidden".into(),
                label: "Hidden".into(),
                owner: hidden,
                actions: MemoryActionSet::all(),
            }]),
        );

        let err = resolve_space_owner(&ctx, Some("hidden"), SpaceDefault::Current).unwrap_err();
        assert!(err.to_string().contains("unknown memory space: hidden"));
        assert!(list_memory_spaces(&ctx).is_empty());
    }
}
