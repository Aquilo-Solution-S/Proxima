use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::mcp::{McpTool, McpToolCtx, McpToolError};
use crate::{AccessScope, Owner, OwnerPrincipalKind};

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
    pub access: MemorySpaceAccessOutput,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct MemorySpaceAccessOutput {
    pub unrestricted: bool,
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
    let access = MemorySpaceAccessOutput {
        unrestricted: ctx.authz.capabilities.access == AccessScope::Unrestricted,
    };
    sorted_accessible_principals(ctx)
        .into_iter()
        .map(|owner| {
            let current = owner == ctx.owner;
            MemorySpaceOutput {
                key: if current {
                    "current".into()
                } else {
                    space_key(&owner)
                },
                label: if current {
                    "Current owner".into()
                } else {
                    space_label(&owner)
                },
                access,
            }
        })
        .collect()
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
/// relation gate.
///
/// # Errors
///
/// Returns `InvalidInput` when a provided key is unknown, or when omitted
/// default resolution has no accessible default Owner.
pub fn resolve_space_owner(
    ctx: &McpToolCtx,
    raw: Option<&str>,
    default: SpaceDefault,
) -> Result<ResolvedMemorySpace, McpToolError> {
    let default_owner = match default {
        SpaceDefault::Current => ctx.owner.clone(),
        SpaceDefault::Identity => ctx.authz.identity.principal.clone(),
    };
    let owner = match raw {
        Some("current") => ctx
            .authz
            .identity
            .can_access_principal(&ctx.owner)
            .then_some(ctx.owner.clone()),
        Some(key) => sorted_accessible_principals(ctx)
            .into_iter()
            .find(|owner| space_key(owner) == key),
        None => ctx
            .authz
            .identity
            .can_access_principal(&default_owner)
            .then_some(default_owner),
    };
    owner
        .map(|owner| {
            let current = owner == ctx.owner;
            ResolvedMemorySpace {
                key: if current {
                    "current".into()
                } else {
                    space_key(&owner)
                },
                label: if current {
                    "Current owner".into()
                } else {
                    space_label(&owner)
                },
                owner,
            }
        })
        .ok_or_else(|| {
            McpToolError::InvalidInput(raw.map_or_else(
                || "current memory space is not accessible".to_string(),
                |key| format!("unknown memory space: {key}"),
            ))
        })
}

fn sorted_accessible_principals(ctx: &McpToolCtx) -> Vec<Owner> {
    let mut owners = ctx
        .authz
        .identity
        .accessible_principals
        .iter()
        .cloned()
        .collect::<Vec<_>>();
    owners.sort_by_key(|owner| {
        let (kind, id) = owner.columns();
        (owner_kind_sort_key(kind), id)
    });
    owners
}

const fn owner_kind_sort_key(kind: OwnerPrincipalKind) -> u8 {
    match kind {
        OwnerPrincipalKind::User => 0,
        OwnerPrincipalKind::Group => 1,
    }
}

fn space_key(owner: &Owner) -> String {
    let (kind, id) = owner.columns();
    match kind {
        OwnerPrincipalKind::User => format!("user:{id}"),
        OwnerPrincipalKind::Group => format!("group:{id}"),
    }
}

fn space_label(owner: &Owner) -> String {
    let (kind, id) = owner.columns();
    match kind {
        OwnerPrincipalKind::User => format!("User {id}"),
        OwnerPrincipalKind::Group => format!("Group {id}"),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::Arc;

    use crate::mcp::{McpAuthorContext, McpTool, McpToolExtensions, OutputMode};
    use crate::{
        AccessScope, AuthPath, AuthzContext, CapabilitySet, FlavorRegistry, GroupId, Identity,
        Principal, ToolScope, UserId,
    };

    use super::*;

    fn make_ctx_with_accessible(owners: Vec<Principal>, access: AccessScope) -> McpToolCtx {
        let principal = owners
            .first()
            .cloned()
            .unwrap_or_else(|| Principal::User(UserId::new(uuid::Uuid::now_v7())));
        let accessible_principals = owners.into_iter().collect::<HashSet<_>>();
        make_ctx(principal.clone(), principal, accessible_principals, access)
    }

    fn make_ctx(
        owner: Principal,
        principal: Principal,
        accessible_principals: HashSet<Principal>,
        access: AccessScope,
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
                    access,
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
    async fn memory_spaces_lists_accessible_principals_without_raw_authority() {
        let personal = Principal::User(UserId::new(uuid::Uuid::now_v7()));
        let shared = Principal::Group(GroupId::new(uuid::Uuid::now_v7()));
        let shared_key = space_key(&shared);
        let ctx =
            make_ctx_with_accessible(vec![personal.clone(), shared], AccessScope::Unrestricted);

        let out = MemorySpacesTool::call(ctx, MemorySpacesArgs {})
            .await
            .unwrap();
        assert_eq!(out.spaces.len(), 2);
        assert_eq!(out.spaces[0].key, "current");
        assert!(out.spaces[0].access.unrestricted);
        assert_eq!(out.spaces[1].key, shared_key);
        assert!(out.spaces[1].access.unrestricted);
    }

    #[tokio::test]
    async fn memory_spaces_granted_scope_reports_grant_gated_access() {
        let personal = Principal::User(UserId::new(uuid::Uuid::now_v7()));
        let ctx = make_ctx_with_accessible(vec![personal], AccessScope::Granted);

        let out = MemorySpacesTool::call(ctx, MemorySpacesArgs {})
            .await
            .unwrap();
        assert_eq!(out.spaces.len(), 1);
        assert_eq!(out.spaces[0].key, "current");
        assert!(!out.spaces[0].access.unrestricted);
    }

    #[test]
    fn resolution_omitted_space_matches_current_owner() {
        let personal = Principal::User(UserId::new(uuid::Uuid::now_v7()));
        let shared = Principal::Group(GroupId::new(uuid::Uuid::now_v7()));
        let ctx =
            make_ctx_with_accessible(vec![personal.clone(), shared], AccessScope::Unrestricted);

        let resolved = resolve_space_owner(&ctx, None, SpaceDefault::Current).unwrap();
        assert_eq!(resolved.key, "current");
        assert_eq!(resolved.owner, personal);
    }

    #[test]
    fn resolution_rejects_owner_without_visibility() {
        let user = Principal::User(UserId::new(uuid::Uuid::now_v7()));
        let hidden = Principal::Group(GroupId::new(uuid::Uuid::now_v7()));
        let mut accessible_principals = HashSet::new();
        accessible_principals.insert(user.clone());
        let ctx = make_ctx(
            user.clone(),
            user,
            accessible_principals,
            AccessScope::Unrestricted,
        );
        let hidden_key = space_key(&hidden);

        let err = resolve_space_owner(&ctx, Some(&hidden_key), SpaceDefault::Current).unwrap_err();
        assert!(
            err.to_string()
                .contains(&format!("unknown memory space: {hidden_key}"))
        );
        assert_eq!(list_memory_spaces(&ctx).len(), 1);
    }
}
