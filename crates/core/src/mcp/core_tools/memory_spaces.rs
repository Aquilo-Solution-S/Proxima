use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::mcp::{McpTool, McpToolCtx, McpToolError};
use crate::{AccessScope, GroupId, Owner, OwnerRef, OwnerRefKind, UserId};

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
        unrestricted: ctx.authz.access_scope() == AccessScope::Unrestricted,
    };
    sorted_accessible_principals(ctx)
        .into_iter()
        .map(|owner| {
            let current = owner == ctx.owner;
            MemorySpaceOutput {
                key: if current {
                    MemorySpaceKey::Current.to_wire()
                } else {
                    MemorySpaceKey::owner(owner).to_wire()
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
        SpaceDefault::Current => ctx.owner,
        SpaceDefault::Identity => ctx.authz.principal(),
    };
    let owner = match raw {
        Some(key) => MemorySpaceKey::parse(key).and_then(|parsed| match parsed {
            MemorySpaceKey::Current => ctx.authz.can_access_owner(&ctx.owner).then_some(ctx.owner),
            MemorySpaceKey::Owner(owner) => ctx.authz.can_access_owner(&owner).then_some(owner),
        }),
        None => ctx
            .authz
            .can_access_owner(&default_owner)
            .then_some(default_owner),
    };
    owner
        .map(|owner| {
            let current = owner == ctx.owner;
            ResolvedMemorySpace {
                key: if current {
                    MemorySpaceKey::Current.to_wire()
                } else {
                    MemorySpaceKey::owner(owner).to_wire()
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
    let mut owners = ctx.authz.accessible_owners().collect::<Vec<_>>();
    owners.sort_by_key(|owner| {
        let (kind, id) = owner.columns();
        (*owner != ctx.owner, owner_kind_sort_key(kind), id)
    });
    owners
}

const fn owner_kind_sort_key(kind: OwnerRefKind) -> u8 {
    match kind {
        OwnerRefKind::World => 0,
        OwnerRefKind::Personal => 1,
        OwnerRefKind::Group => 2,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MemorySpaceKey {
    Current,
    Owner(Owner),
}

impl MemorySpaceKey {
    const CURRENT: &'static str = "current";
    const WORLD: &'static str = "world";
    const PERSONAL_PREFIX: &'static str = "personal:";
    const GROUP_PREFIX: &'static str = "group:";

    #[must_use]
    pub const fn owner(owner: Owner) -> Self {
        Self::Owner(owner)
    }

    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        if raw == Self::CURRENT {
            return Some(Self::Current);
        }
        if raw == Self::WORLD {
            return Some(Self::Owner(OwnerRef::World));
        }
        if let Some(id) = raw.strip_prefix(Self::PERSONAL_PREFIX) {
            return uuid::Uuid::parse_str(id)
                .ok()
                .map(|id| Self::Owner(OwnerRef::Personal(UserId::new(id))));
        }
        raw.strip_prefix(Self::GROUP_PREFIX).and_then(|id| {
            uuid::Uuid::parse_str(id)
                .ok()
                .map(|id| Self::Owner(OwnerRef::Group(GroupId::new(id))))
        })
    }

    #[must_use]
    pub fn to_wire(self) -> String {
        match self {
            Self::Current => Self::CURRENT.to_string(),
            Self::Owner(owner) => match owner {
                OwnerRef::World => Self::WORLD.to_string(),
                OwnerRef::Personal(user) => {
                    format!("{}{}", Self::PERSONAL_PREFIX, user.into_inner())
                }
                OwnerRef::Group(group) => format!("{}{}", Self::GROUP_PREFIX, group.into_inner()),
            },
        }
    }
}

pub(crate) fn space_label(owner: &Owner) -> String {
    match owner {
        OwnerRef::World => "World".to_string(),
        OwnerRef::Personal(user) => format!("Personal {}", user.into_inner()),
        OwnerRef::Group(group) => format!("Group {}", group.into_inner()),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::Arc;

    use crate::mcp::{McpAuthorContext, McpTool, McpToolExtensions, OutputMode};
    use crate::{
        AccessScope, AuthPath, AuthzContext, FlavorRegistry, GroupId, OwnerRef, ToolScope, UserId,
    };

    use super::*;

    fn make_ctx_with_accessible(owners: Vec<OwnerRef>, access: AccessScope) -> McpToolCtx {
        let principal = owners
            .first()
            .copied()
            .unwrap_or_else(|| OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7())));
        let accessible_principals = owners.into_iter().collect::<HashSet<_>>();
        make_ctx(principal, principal, accessible_principals, access)
    }

    fn make_ctx(
        owner: OwnerRef,
        principal: OwnerRef,
        accessible_principals: HashSet<OwnerRef>,
        access: AccessScope,
    ) -> McpToolCtx {
        McpToolCtx {
            owner,
            authz: AuthzContext::scoped_access(
                principal,
                accessible_principals,
                ToolScope::All,
                access,
                AuthPath::HostBearer,
            ),
            handles: None,
            mode: OutputMode::PrefixedIds,
            registry: Arc::new(FlavorRegistry::new().freeze()),
            author: McpAuthorContext {
                model_id: "test".into(),
                client_name: "test".into(),
                client_version: "0".into(),
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
        let personal = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
        let shared = OwnerRef::Group(GroupId::new(uuid::Uuid::now_v7()));
        let shared_key = MemorySpaceKey::owner(shared).to_wire();
        let ctx = make_ctx_with_accessible(vec![personal, shared], AccessScope::Unrestricted);

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
        let personal = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
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
        let personal = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
        let shared = OwnerRef::Group(GroupId::new(uuid::Uuid::now_v7()));
        let ctx = make_ctx_with_accessible(vec![personal, shared], AccessScope::Unrestricted);

        let resolved = resolve_space_owner(&ctx, None, SpaceDefault::Current).unwrap();
        assert_eq!(resolved.key, "current");
        assert_eq!(resolved.owner, personal);
    }

    #[test]
    fn resolution_rejects_owner_without_visibility() {
        let user = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
        let hidden = OwnerRef::Group(GroupId::new(uuid::Uuid::now_v7()));
        let mut accessible_principals = HashSet::new();
        accessible_principals.insert(user);
        let ctx = make_ctx(user, user, accessible_principals, AccessScope::Unrestricted);
        let hidden_key = MemorySpaceKey::owner(hidden).to_wire();

        let err = resolve_space_owner(&ctx, Some(&hidden_key), SpaceDefault::Current).unwrap_err();
        assert!(
            err.to_string()
                .contains(&format!("unknown memory space: {hidden_key}"))
        );
        assert_eq!(list_memory_spaces(&ctx).len(), 1);
    }
}
