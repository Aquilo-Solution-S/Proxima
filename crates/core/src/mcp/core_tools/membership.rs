use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::access::Relation;
use crate::authz::AuthzContext;
use crate::engine::GroupMemberPage;
use crate::error::ProtocolError;
use crate::mcp::cursor as wire_cursor;
use crate::mcp::{CoreActionMeta, McpActionArgSpec, McpTool, McpToolCtx, McpToolError};
use crate::protocol::{action as protocol_action, tool as protocol_tool};
use crate::{GroupId, OwnerRef, UserId};

/// Opaque cursor codec: the shared `{v, fp, c}` envelope with the last
/// `(member, relation)` pair under `c`. The fingerprint binds the group.
const MEMBER_CURSOR: wire_cursor::FingerprintedCursor = wire_cursor::FingerprintedCursor {
    version: 1,
    source: "core_membership list_members response",
    rebind_hint: "repeat the group that produced it",
};

use super::memory_spaces::MemorySpaceKey;
use super::{DESTRUCTIVE_NON_IDEMPOTENT, READ_ONLY, WRITE_NON_IDEMPOTENT};

pub const CORE_MEMBERSHIP_ACTIONS: &[CoreActionMeta] = &[
    CoreActionMeta {
        tool: CoreMembershipTool::NAME,
        action: "add_member",
        scope_key: protocol_action::CORE_MEMBERSHIP_ADD_MEMBER,
        description: "Add one user membership relation to a Group space.",
        produces_schema_ids: &[],
        annotations: WRITE_NON_IDEMPOTENT,
    },
    CoreActionMeta {
        tool: CoreMembershipTool::NAME,
        action: "remove_member",
        scope_key: protocol_action::CORE_MEMBERSHIP_REMOVE_MEMBER,
        description: "Remove all membership relations for one user in a Group space.",
        produces_schema_ids: &[],
        annotations: DESTRUCTIVE_NON_IDEMPOTENT,
    },
    CoreActionMeta {
        tool: CoreMembershipTool::NAME,
        action: "list_members",
        scope_key: protocol_action::CORE_MEMBERSHIP_LIST_MEMBERS,
        description: "List users and relations for one Group space.",
        produces_schema_ids: &[],
        annotations: READ_ONLY,
    },
];

#[derive(Debug, Default)]
pub struct CoreMembershipTool;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum CoreMembershipArgs {
    AddMember(AddMemberArgs),
    RemoveMember(RemoveMemberArgs),
    ListMembers(ListMembersArgs),
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AddMemberArgs {
    /// Group space key from `core_memory_spaces`, e.g. `group:<uuid>`.
    pub group: String,
    /// User UUID string. Users are not MCP entities and take no prefix.
    pub member: String,
    /// Membership relation (case-insensitive): `admin`, `editor`,
    /// `viewer`, or `ingest`.
    pub relation: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RemoveMemberArgs {
    /// Group space key from `core_memory_spaces`, e.g. `group:<uuid>`.
    pub group: String,
    /// User UUID string. All relations for this member in the group are removed.
    pub member: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListMembersArgs {
    /// Group space key from `core_memory_spaces`, e.g. `group:<uuid>`.
    pub group: String,
    /// Max members per page; clamped to 1..=200, default 50.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Opaque pagination cursor from a previous response's `next_cursor`.
    /// The group must stay unchanged between pages; `limit` may vary.
    #[serde(default)]
    pub cursor: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum CoreMembershipOutput {
    AddMember(MutationOutput),
    RemoveMember(MutationOutput),
    ListMembers(ListMembersOutput),
}

#[derive(Debug, Serialize)]
pub struct MutationOutput {
    pub ok: bool,
}

#[derive(Debug, Serialize)]
pub struct ListMembersOutput {
    pub members: Vec<MemberOutput>,
    pub next_cursor: Option<String>,
    pub has_more: bool,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct MemberOutput {
    pub member: String,
    pub relation: String,
}

/// Keyset resume point carried inside the opaque member cursor.
#[derive(Debug, Serialize, Deserialize)]
struct MemberCursorPos {
    member: uuid::Uuid,
    relation: String,
}

fn member_fingerprint(group: GroupId) -> String {
    let canon = serde_json::to_string(&group.into_inner()).expect("fingerprint canon serializes");
    wire_cursor::fingerprint(&canon)
}

impl McpTool for CoreMembershipTool {
    const NAME: &'static str = protocol_tool::CORE_MEMBERSHIP;
    const DESCRIPTION: &'static str =
        "Membership dispatcher — add_member/remove_member/list_members.";
    const ACTION_ARG_SPECS: &'static [McpActionArgSpec] = &[
        McpActionArgSpec {
            action: "add_member",
            allowed_fields: &["group", "member", "relation"],
            required_fields: &["group", "member", "relation"],
        },
        McpActionArgSpec {
            action: "remove_member",
            allowed_fields: &["group", "member"],
            required_fields: &["group", "member"],
        },
        McpActionArgSpec {
            action: "list_members",
            allowed_fields: &["group", "limit", "cursor"],
            required_fields: &["group"],
        },
    ];
    type Args = CoreMembershipArgs;
    type Output = CoreMembershipOutput;

    fn call(
        ctx: McpToolCtx,
        args: CoreMembershipArgs,
    ) -> BoxFuture<'static, Result<CoreMembershipOutput, McpToolError>> {
        Box::pin(async move {
            let engine = ctx.require_engine()?;
            execute_membership(engine, &ctx, args).await
        })
    }
}

trait MembershipEngine: Sync {
    fn add_member<'a>(
        &'a self,
        authz: &'a AuthzContext,
        group: GroupId,
        member: UserId,
        relation: Relation,
    ) -> BoxFuture<'a, Result<(), ProtocolError>>;

    fn remove_member<'a>(
        &'a self,
        authz: &'a AuthzContext,
        group: GroupId,
        member: UserId,
    ) -> BoxFuture<'a, Result<(), ProtocolError>>;

    fn list_members<'a>(
        &'a self,
        authz: &'a AuthzContext,
        group: GroupId,
        limit: u32,
        after: Option<(UserId, Relation)>,
    ) -> BoxFuture<'a, Result<GroupMemberPage, ProtocolError>>;
}

impl MembershipEngine for crate::Engine {
    fn add_member<'a>(
        &'a self,
        authz: &'a AuthzContext,
        group: GroupId,
        member: UserId,
        relation: Relation,
    ) -> BoxFuture<'a, Result<(), ProtocolError>> {
        Box::pin(crate::Engine::add_member(
            self, authz, group, member, relation,
        ))
    }

    fn remove_member<'a>(
        &'a self,
        authz: &'a AuthzContext,
        group: GroupId,
        member: UserId,
    ) -> BoxFuture<'a, Result<(), ProtocolError>> {
        Box::pin(crate::Engine::remove_member(self, authz, group, member))
    }

    fn list_members<'a>(
        &'a self,
        authz: &'a AuthzContext,
        group: GroupId,
        limit: u32,
        after: Option<(UserId, Relation)>,
    ) -> BoxFuture<'a, Result<GroupMemberPage, ProtocolError>> {
        Box::pin(crate::Engine::list_members(
            self, authz, group, limit, after,
        ))
    }
}

async fn execute_membership(
    engine: &dyn MembershipEngine,
    ctx: &McpToolCtx,
    args: CoreMembershipArgs,
) -> Result<CoreMembershipOutput, McpToolError> {
    match args {
        CoreMembershipArgs::AddMember(args) => {
            let group = resolve_group(ctx, &args.group)?;
            let member = parse_user_id(&args.member)?;
            let relation = parse_relation(&args.relation)?;
            engine
                .add_member(&ctx.authz, group, member, relation)
                .await?;
            Ok(CoreMembershipOutput::AddMember(MutationOutput { ok: true }))
        }
        CoreMembershipArgs::RemoveMember(args) => {
            let group = resolve_group(ctx, &args.group)?;
            let member = parse_user_id(&args.member)?;
            engine.remove_member(&ctx.authz, group, member).await?;
            Ok(CoreMembershipOutput::RemoveMember(MutationOutput {
                ok: true,
            }))
        }
        CoreMembershipArgs::ListMembers(args) => {
            let group = resolve_group(ctx, &args.group)?;
            let limit = super::clamp_page_limit(args.limit);
            let fingerprint = member_fingerprint(group);
            let after = args
                .cursor
                .as_deref()
                .map(|raw| {
                    let pos: MemberCursorPos = MEMBER_CURSOR.decode(&fingerprint, raw)?;
                    Ok::<_, McpToolError>((UserId::new(pos.member), parse_relation(&pos.relation)?))
                })
                .transpose()?;
            let page = engine.list_members(&ctx.authz, group, limit, after).await?;
            let next_cursor = (page.has_more && !page.members.is_empty()).then(|| {
                let (member, relation) = page.members.last().expect("non-empty page");
                MEMBER_CURSOR.encode(
                    &fingerprint,
                    &MemberCursorPos {
                        member: member.into_inner(),
                        relation: format_relation(*relation).to_string(),
                    },
                )
            });
            let members = page
                .members
                .into_iter()
                .map(|(member, relation)| MemberOutput {
                    member: member.into_inner().to_string(),
                    relation: format_relation(relation).to_string(),
                })
                .collect();
            Ok(CoreMembershipOutput::ListMembers(ListMembersOutput {
                members,
                next_cursor,
                has_more: page.has_more,
            }))
        }
    }
}

/// Parse a group-space key into a bare `GroupId`, structurally only.
///
/// Deliberately bypasses `resolve_space_owner`'s `can_access_owner`
/// pre-filter: that filter answers "which spaces can I currently author
/// into", which is the wrong question for membership administration. The
/// caller need not already be visible into the group to be told they lack
/// admin authority over it — `Engine::{add,remove,list}_member`'s
/// `authorize_write` gate is the sole, authoritative decision, and it must
/// surface as `Forbidden`, not an earlier `InvalidInput` that hides the
/// real reason behind an existence-style non-answer.
fn resolve_group(ctx: &McpToolCtx, raw: &str) -> Result<GroupId, McpToolError> {
    let owner = match MemorySpaceKey::parse(raw) {
        Some(MemorySpaceKey::Current) => ctx.owner,
        Some(MemorySpaceKey::Owner(owner)) => owner,
        None => {
            return Err(McpToolError::InvalidInput(format!(
                "unknown memory space: {raw}"
            )));
        }
    };
    match owner {
        OwnerRef::Group(group) => Ok(group),
        OwnerRef::World | OwnerRef::Personal(_) => {
            Err(McpToolError::InvalidInput("not a group".into()))
        }
    }
}

fn parse_user_id(raw: &str) -> Result<UserId, McpToolError> {
    raw.parse::<uuid::Uuid>()
        .map(UserId::new)
        .map_err(|err| McpToolError::InvalidInput(format!("member is not a user uuid: {err}")))
}

/// Casing is not signal for enum-like string args anywhere on the tool
/// surface (schema kinds, goal states, search modes all fold case), so
/// the relation arg folds it too; unknown relations still fail closed.
fn parse_relation(raw: &str) -> Result<Relation, McpToolError> {
    match raw.to_ascii_lowercase().as_str() {
        "admin" => Ok(Relation::Admin),
        "editor" => Ok(Relation::Editor),
        "viewer" => Ok(Relation::Viewer),
        "ingest" => Ok(Relation::Ingest),
        _ => Err(McpToolError::InvalidInput(
            "relation must be one of: admin, editor, viewer, ingest".into(),
        )),
    }
}

fn format_relation(relation: Relation) -> &'static str {
    match relation {
        Relation::Admin => "admin",
        Relation::Editor => "editor",
        Relation::Viewer => "viewer",
        Relation::Ingest => "ingest",
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use crate::access::Role;
    use crate::mcp::core_tools::memory_spaces::MemorySpaceKey;
    use crate::mcp::{McpAuthorContext, McpToolExtensions, validate_action_args};
    use crate::{AuthPath, AuthzContext, FlavorRegistry};

    use super::*;

    #[derive(Default)]
    struct MockMembershipEngine {
        added: Mutex<Vec<(GroupId, UserId, Relation)>>,
        removed: Mutex<Vec<(GroupId, UserId)>>,
        members: Mutex<Vec<(UserId, Relation)>>,
    }

    impl MembershipEngine for MockMembershipEngine {
        fn add_member<'a>(
            &'a self,
            _authz: &'a AuthzContext,
            group: GroupId,
            member: UserId,
            relation: Relation,
        ) -> BoxFuture<'a, Result<(), ProtocolError>> {
            Box::pin(async move {
                self.added
                    .lock()
                    .expect("added lock")
                    .push((group, member, relation));
                Ok(())
            })
        }

        fn remove_member<'a>(
            &'a self,
            _authz: &'a AuthzContext,
            group: GroupId,
            member: UserId,
        ) -> BoxFuture<'a, Result<(), ProtocolError>> {
            Box::pin(async move {
                self.removed
                    .lock()
                    .expect("removed lock")
                    .push((group, member));
                Ok(())
            })
        }

        fn list_members<'a>(
            &'a self,
            _authz: &'a AuthzContext,
            _group: GroupId,
            limit: u32,
            after: Option<(UserId, Relation)>,
        ) -> BoxFuture<'a, Result<GroupMemberPage, ProtocolError>> {
            Box::pin(async move {
                let all = self.members.lock().expect("members lock").clone();
                let start = after.map_or(0, |pos| {
                    all.iter()
                        .position(|entry| *entry == pos)
                        .map_or(all.len(), |found| found + 1)
                });
                let rest = &all[start..];
                let page_len = rest.len().min(usize::try_from(limit).unwrap_or(usize::MAX));
                Ok(GroupMemberPage {
                    members: rest[..page_len].to_vec(),
                    has_more: rest.len() > page_len,
                })
            })
        }
    }

    fn ctx_with_principals(owner: OwnerRef, accessible: Vec<OwnerRef>) -> McpToolCtx {
        let OwnerRef::Personal(subject) = owner else {
            panic!("ctx_with_principals requires a personal owner");
        };
        // Server-resolved: the caller manages (admin) every group it can reach,
        // plus its own personal owner and World (viewer). Faithful to the old
        // unrestricted-over-accessible semantics for these membership tests.
        let group_roles = accessible
            .into_iter()
            .filter_map(|principal| match principal {
                OwnerRef::Group(_) => Some((principal, Role::admin())),
                OwnerRef::Personal(_) | OwnerRef::World => None,
            });
        McpToolCtx {
            owner,
            authz: AuthzContext::for_subject_with_role(subject, group_roles, AuthPath::HostBearer),
            registry: Arc::new(FlavorRegistry::new().freeze_or_panic_for_tests()),
            author: McpAuthorContext {
                model_id: "test".into(),
                client_name: "test".into(),
                client_version: "0".into(),
                caller_self_perspective: None,
            },
            caller_self_perspective: None,
            extensions: McpToolExtensions::default(),
            engine: None,
        }
    }

    #[tokio::test]
    async fn add_member_routes_to_engine_with_parsed_relation() {
        let owner = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
        let group = GroupId::new(uuid::Uuid::now_v7());
        let group_owner = OwnerRef::Group(group);
        let ctx = ctx_with_principals(owner, vec![group_owner]);
        let member = UserId::new(uuid::Uuid::now_v7());
        let engine = MockMembershipEngine::default();

        let out = execute_membership(
            &engine,
            &ctx,
            CoreMembershipArgs::AddMember(AddMemberArgs {
                group: MemorySpaceKey::owner(group_owner).to_wire(),
                member: member.into_inner().to_string(),
                relation: "editor".into(),
            }),
        )
        .await
        .expect("add_member routes");

        assert!(matches!(
            out,
            CoreMembershipOutput::AddMember(MutationOutput { ok: true })
        ));
        assert_eq!(
            engine.added.lock().expect("added lock").as_slice(),
            &[(group, member, Relation::Editor)]
        );
    }

    #[test]
    fn unknown_action_is_invalid_input() {
        let err = validate_action_args(
            CoreMembershipTool::NAME,
            CoreMembershipTool::ACTION_ARG_SPECS,
            &serde_json::json!({"action": "bogus"}),
        )
        .expect_err("unknown action rejected");

        assert!(matches!(err, McpToolError::InvalidInput(_)));
    }

    #[test]
    fn publish_to_world_action_is_invalid_input() {
        let err = validate_action_args(
            CoreMembershipTool::NAME,
            CoreMembershipTool::ACTION_ARG_SPECS,
            &serde_json::json!({"action": "publish_to_world", "entity": "F:00000000-0000-0000-0000-000000000001"}),
        )
        .expect_err("publish_to_world is no longer a membership action");

        assert!(matches!(err, McpToolError::InvalidInput(_)));
    }

    #[tokio::test]
    async fn non_group_group_is_invalid_input() {
        let owner = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
        let ctx = ctx_with_principals(owner, vec![owner]);
        let member = UserId::new(uuid::Uuid::now_v7());
        let engine = MockMembershipEngine::default();

        let err = execute_membership(
            &engine,
            &ctx,
            CoreMembershipArgs::AddMember(AddMemberArgs {
                group: "current".into(),
                member: member.into_inner().to_string(),
                relation: "viewer".into(),
            }),
        )
        .await
        .expect_err("current user is not a group");

        assert!(matches!(err, McpToolError::InvalidInput(message) if message == "not a group"));
    }

    #[test]
    fn bad_relation_is_invalid_input() {
        let err = parse_relation("owner").expect_err("bad relation rejected");

        assert!(matches!(err, McpToolError::InvalidInput(message) if message.contains("relation")));
    }

    #[test]
    fn relation_parse_is_case_insensitive() {
        for spelling in ["editor", "Editor", "EDITOR"] {
            assert!(matches!(
                parse_relation(spelling).expect("valid relation"),
                Relation::Editor
            ));
        }
        // Folding case must not widen the accepted set.
        assert!(parse_relation("OWNER").is_err());
    }

    /// Pages walk the whole membership exactly once via the opaque
    /// cursor, and the cursor fails closed against a different group.
    #[tokio::test]
    async fn list_members_pages_with_group_bound_cursor() {
        let owner = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
        let group = GroupId::new(uuid::Uuid::now_v7());
        let group_owner = OwnerRef::Group(group);
        let ctx = ctx_with_principals(owner, vec![group_owner]);
        let engine = MockMembershipEngine::default();
        let all: Vec<(UserId, Relation)> = (0..3)
            .map(|_| (UserId::new(uuid::Uuid::now_v7()), Relation::Viewer))
            .collect();
        *engine.members.lock().expect("members lock") = all.clone();

        let group_key = MemorySpaceKey::owner(group_owner).to_wire();
        let first = execute_membership(
            &engine,
            &ctx,
            CoreMembershipArgs::ListMembers(ListMembersArgs {
                group: group_key.clone(),
                limit: Some(2),
                cursor: None,
            }),
        )
        .await
        .expect("first page");
        let CoreMembershipOutput::ListMembers(first) = first else {
            panic!("expected list output");
        };
        assert_eq!(first.members.len(), 2);
        assert!(first.has_more);
        let token = first.next_cursor.expect("cursor on truncated page");

        let second = execute_membership(
            &engine,
            &ctx,
            CoreMembershipArgs::ListMembers(ListMembersArgs {
                group: group_key,
                limit: Some(2),
                cursor: Some(token.clone()),
            }),
        )
        .await
        .expect("second page");
        let CoreMembershipOutput::ListMembers(second) = second else {
            panic!("expected list output");
        };
        assert_eq!(second.members.len(), 1);
        assert!(!second.has_more);
        assert!(second.next_cursor.is_none());

        let mut walked: Vec<String> = first
            .members
            .iter()
            .chain(second.members.iter())
            .map(|member| member.member.clone())
            .collect();
        walked.sort_unstable();
        let mut expected: Vec<String> = all
            .iter()
            .map(|(member, _)| member.into_inner().to_string())
            .collect();
        expected.sort_unstable();
        assert_eq!(walked, expected, "pages must cover every member once");

        // A cursor minted for one group must fail closed on another.
        let other_group = OwnerRef::Group(GroupId::new(uuid::Uuid::now_v7()));
        let ctx = ctx_with_principals(owner, vec![group_owner, other_group]);
        let err = execute_membership(
            &engine,
            &ctx,
            CoreMembershipArgs::ListMembers(ListMembersArgs {
                group: MemorySpaceKey::owner(other_group).to_wire(),
                limit: Some(2),
                cursor: Some(token),
            }),
        )
        .await
        .expect_err("foreign-group cursor must fail closed");
        assert!(matches!(
            err,
            McpToolError::InvalidInput(message) if message.contains("cursor does not match")
        ));
    }
}
