use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::mcp::{McpTool, McpToolCtx, McpToolError};
use crate::protocol::tool as protocol_tool;
use crate::{AccessKind, GroupId, Owner, OwnerRef, OwnerRefKind, UserId};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MemorySpacesArgs {}

#[derive(Debug, Serialize, JsonSchema)]
pub struct MemorySpacesOutput {
    pub spaces: Vec<MemorySpaceOutput>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct MemorySpaceOutput {
    pub key: String,
    pub label: String,
    pub access: MemorySpaceAccessOutput,
}

#[derive(Debug, Clone, Copy, Serialize, JsonSchema)]
pub struct MemorySpaceAccessOutput {
    /// Whether the caller holds write authority (Fact-write) on this space, as
    /// opposed to read-only visibility. Resolved per space from the caller's
    /// host-resolved owner roles (there is no blanket "unrestricted" mode).
    pub writable: bool,
}

#[derive(Debug)]
pub struct MemorySpacesTool;

impl McpTool for MemorySpacesTool {
    const NAME: &'static str = protocol_tool::CORE_MEMORY_SPACES;
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
                access: MemorySpaceAccessOutput {
                    writable: ctx.authz.may_write(&owner, AccessKind::Fact),
                },
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

/// Engine-free `McpToolCtx` builders shared by unit tests across the core
/// tools (space resolution has no storage dependency).
#[cfg(test)]
pub(crate) mod test_ctx {
    use std::sync::Arc;

    use crate::access::Role;
    use crate::mcp::{McpAuthorContext, McpToolCtx};
    use crate::{AuthPath, AuthzContext, FlavorRegistry, FlavorServices, OwnerRef, UserId};

    /// Build a server-resolved caller context: personal role on `subject`'s own
    /// owner, World viewer, plus the given per-group roles.
    pub(crate) fn ctx_for(subject: UserId, group_roles: Vec<(OwnerRef, Role)>) -> McpToolCtx {
        make_ctx(
            OwnerRef::Personal(subject),
            AuthzContext::for_subject_with_role(subject, group_roles, AuthPath::HostBearer),
        )
    }

    pub(crate) fn make_ctx(owner: OwnerRef, authz: AuthzContext) -> McpToolCtx {
        McpToolCtx {
            owner,
            authz,
            registry: Arc::new(FlavorRegistry::new().freeze_or_panic_for_tests()),
            author: McpAuthorContext {
                model_id: "test".into(),
                client_name: "test".into(),
                client_version: "0".into(),
                caller_self_perspective: None,
            },
            caller_self_perspective: None,
            services: FlavorServices::default(),
            engine: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::access::Role;
    use crate::mcp::McpTool;
    use crate::{GroupId, OwnerRef, UserId};

    use super::test_ctx::ctx_for;
    use super::*;

    fn writable_for(spaces: &[MemorySpaceOutput], key: &str) -> bool {
        spaces
            .iter()
            .find(|space| space.key == key)
            .unwrap_or_else(|| panic!("space {key} present"))
            .access
            .writable
    }

    #[tokio::test]
    async fn memory_spaces_reports_per_space_writability() {
        let subject = UserId::new(uuid::Uuid::now_v7());
        let shared = OwnerRef::Group(GroupId::new(uuid::Uuid::now_v7()));
        let shared_key = MemorySpaceKey::owner(shared).to_wire();
        let world_key = MemorySpaceKey::owner(OwnerRef::World).to_wire();
        let ctx = ctx_for(subject, vec![(shared, Role::editor())]);

        let out = MemorySpacesTool::call(ctx, MemorySpacesArgs {})
            .await
            .unwrap();
        // current (own personal) sorts first; World and the editor group follow.
        assert_eq!(out.spaces[0].key, "current");
        assert!(writable_for(&out.spaces, "current"));
        // Editor on the group → writable; World is public-read, never writable.
        assert!(writable_for(&out.spaces, &shared_key));
        assert!(!writable_for(&out.spaces, &world_key));
    }

    #[tokio::test]
    async fn memory_spaces_viewer_group_is_not_writable() {
        let subject = UserId::new(uuid::Uuid::now_v7());
        let shared = OwnerRef::Group(GroupId::new(uuid::Uuid::now_v7()));
        let shared_key = MemorySpaceKey::owner(shared).to_wire();
        let ctx = ctx_for(subject, vec![(shared, Role::viewer())]);

        let out = MemorySpacesTool::call(ctx, MemorySpacesArgs {})
            .await
            .unwrap();
        // Read-only member: the group space is visible but not writable; the
        // caller's own space still is.
        assert!(writable_for(&out.spaces, "current"));
        assert!(!writable_for(&out.spaces, &shared_key));
    }

    #[test]
    fn resolution_omitted_space_matches_current_owner() {
        let subject = UserId::new(uuid::Uuid::now_v7());
        let shared = OwnerRef::Group(GroupId::new(uuid::Uuid::now_v7()));
        let ctx = ctx_for(subject, vec![(shared, Role::editor())]);

        let resolved = resolve_space_owner(&ctx, None, SpaceDefault::Current).unwrap();
        assert_eq!(resolved.key, "current");
        assert_eq!(resolved.owner, OwnerRef::Personal(subject));
    }

    #[test]
    fn resolution_accepts_explicit_owner_keys() {
        let subject = UserId::new(uuid::Uuid::now_v7());
        let shared = OwnerRef::Group(GroupId::new(uuid::Uuid::now_v7()));
        let ctx = ctx_for(subject, vec![(shared, Role::viewer())]);

        let world = resolve_space_owner(&ctx, Some("world"), SpaceDefault::Current).unwrap();
        assert_eq!(world.owner, OwnerRef::World);
        assert_eq!(world.key, "world");

        let group_key = MemorySpaceKey::owner(shared).to_wire();
        let group = resolve_space_owner(&ctx, Some(&group_key), SpaceDefault::Current).unwrap();
        assert_eq!(group.owner, shared);
        assert_eq!(group.key, group_key);

        // The caller's own space canonicalizes to `current` even when
        // named by its explicit personal:<uuid> spelling.
        let own_key = MemorySpaceKey::owner(OwnerRef::Personal(subject)).to_wire();
        let own = resolve_space_owner(&ctx, Some(&own_key), SpaceDefault::Current).unwrap();
        assert_eq!(own.owner, OwnerRef::Personal(subject));
        assert_eq!(own.key, "current");
    }

    #[test]
    fn identity_default_resolves_to_principal_not_space_owner() {
        let subject = UserId::new(uuid::Uuid::now_v7());
        let shared = OwnerRef::Group(GroupId::new(uuid::Uuid::now_v7()));
        let authz = crate::AuthzContext::for_subject_with_role(
            subject,
            vec![(shared, Role::editor())],
            crate::AuthPath::HostBearer,
        );
        let ctx = super::test_ctx::make_ctx(shared, authz);

        // An omitted key with the Current default follows the session's
        // space owner (here a group)...
        let current = resolve_space_owner(&ctx, None, SpaceDefault::Current).unwrap();
        assert_eq!(current.owner, shared);
        assert_eq!(current.key, "current");
        // ...while the Identity default follows the authenticated
        // principal regardless of which space the session is bound to.
        let identity = resolve_space_owner(&ctx, None, SpaceDefault::Identity).unwrap();
        assert_eq!(identity.owner, OwnerRef::Personal(subject));
    }

    #[test]
    fn space_key_wire_round_trip_covers_every_owner_kind() {
        for owner in [
            OwnerRef::World,
            OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7())),
            OwnerRef::Group(GroupId::new(uuid::Uuid::now_v7())),
        ] {
            let wire = MemorySpaceKey::owner(owner).to_wire();
            assert_eq!(
                MemorySpaceKey::parse(&wire),
                Some(MemorySpaceKey::Owner(owner)),
                "round trip for {wire}",
            );
        }
        assert_eq!(
            MemorySpaceKey::parse("current"),
            Some(MemorySpaceKey::Current)
        );
        // Unknown prefixes and malformed uuids fail closed.
        assert_eq!(MemorySpaceKey::parse("personal:not-a-uuid"), None);
        assert_eq!(MemorySpaceKey::parse("tenant:123"), None);
    }

    #[test]
    fn resolution_rejects_owner_without_visibility() {
        let subject = UserId::new(uuid::Uuid::now_v7());
        let hidden = OwnerRef::Group(GroupId::new(uuid::Uuid::now_v7()));
        let ctx = ctx_for(subject, vec![]);
        let hidden_key = MemorySpaceKey::owner(hidden).to_wire();

        let err = resolve_space_owner(&ctx, Some(&hidden_key), SpaceDefault::Current).unwrap_err();
        assert!(
            err.to_string()
                .contains(&format!("unknown memory space: {hidden_key}"))
        );
        // A bare subject sees only its own space + World (public read).
        assert_eq!(list_memory_spaces(&ctx).len(), 2);
    }
}
