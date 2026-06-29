use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::access::Relation;
use crate::authz::AuthzContext;
use crate::error::ProtocolError;
use crate::mcp::{CoreActionMeta, McpActionArgSpec, McpTool, McpToolCtx, McpToolError};
use crate::{GroupId, OwnerRef, UserId};

use super::memory_spaces::{SpaceDefault, resolve_space_owner};
use super::{DESTRUCTIVE_NON_IDEMPOTENT, READ_ONLY, WRITE_NON_IDEMPOTENT};

const CORE_MEMBERSHIP_ADD_MEMBER_SCOPE_KEY: &str = "core_membership:add_member";
const CORE_MEMBERSHIP_REMOVE_MEMBER_SCOPE_KEY: &str = "core_membership:remove_member";
const CORE_MEMBERSHIP_LIST_MEMBERS_SCOPE_KEY: &str = "core_membership:list_members";

pub const CORE_MEMBERSHIP_ACTIONS: &[CoreActionMeta] = &[
    CoreActionMeta {
        tool: CoreMembershipTool::NAME,
        action: "add_member",
        scope_key: CORE_MEMBERSHIP_ADD_MEMBER_SCOPE_KEY,
        description: "Add one user membership relation to a Group space.",
        produces_schema_ids: &[],
        annotations: WRITE_NON_IDEMPOTENT,
    },
    CoreActionMeta {
        tool: CoreMembershipTool::NAME,
        action: "remove_member",
        scope_key: CORE_MEMBERSHIP_REMOVE_MEMBER_SCOPE_KEY,
        description: "Remove all membership relations for one user in a Group space.",
        produces_schema_ids: &[],
        annotations: DESTRUCTIVE_NON_IDEMPOTENT,
    },
    CoreActionMeta {
        tool: CoreMembershipTool::NAME,
        action: "list_members",
        scope_key: CORE_MEMBERSHIP_LIST_MEMBERS_SCOPE_KEY,
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
    /// User UUID string. Users have no MCP handle system.
    pub member: String,
    /// Membership relation: `admin`, `editor`, `viewer`, or `ingest`.
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
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum CoreMembershipOutput {
    AddMember(MutationOutput),
    RemoveMember(MutationOutput),
    ListMembers(Vec<MemberOutput>),
}

#[derive(Debug, Serialize)]
pub struct MutationOutput {
    pub ok: bool,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct MemberOutput {
    pub member: String,
    pub relation: String,
}

impl McpTool for CoreMembershipTool {
    const NAME: &'static str = "core_membership";
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
            allowed_fields: &["group"],
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
            let engine = ctx
                .engine()
                .ok_or_else(|| McpToolError::Other("engine unavailable".into()))?;
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
    ) -> BoxFuture<'a, Result<Vec<(UserId, Relation)>, ProtocolError>>;
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
    ) -> BoxFuture<'a, Result<Vec<(UserId, Relation)>, ProtocolError>> {
        Box::pin(crate::Engine::list_members(self, authz, group))
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
            let members = engine
                .list_members(&ctx.authz, group)
                .await?
                .into_iter()
                .map(|(member, relation)| MemberOutput {
                    member: member.into_inner().to_string(),
                    relation: format_relation(relation).to_string(),
                })
                .collect();
            Ok(CoreMembershipOutput::ListMembers(members))
        }
    }
}

fn resolve_group(ctx: &McpToolCtx, raw: &str) -> Result<GroupId, McpToolError> {
    let owner = resolve_space_owner(ctx, Some(raw), SpaceDefault::Current)?.owner;
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

fn parse_relation(raw: &str) -> Result<Relation, McpToolError> {
    match raw {
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

    use crate::mcp::core_tools::memory_spaces::MemorySpaceKey;
    use crate::mcp::{McpAuthorContext, McpToolExtensions, OutputMode, validate_action_args};
    use crate::{AccessScope, AuthPath, AuthzContext, FlavorRegistry, ToolScope};

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
        ) -> BoxFuture<'a, Result<Vec<(UserId, Relation)>, ProtocolError>> {
            Box::pin(async move { Ok(self.members.lock().expect("members lock").clone()) })
        }
    }

    fn ctx_with_principals(owner: OwnerRef, accessible: Vec<OwnerRef>) -> McpToolCtx {
        McpToolCtx {
            owner,
            authz: AuthzContext::scoped_access(
                owner,
                accessible,
                ToolScope::All,
                AccessScope::Unrestricted,
                AuthPath::HostBearer,
            ),
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
}
