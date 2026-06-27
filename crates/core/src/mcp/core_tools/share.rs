use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::access::EntityId;
use crate::authz::AuthzContext;
use crate::error::ProtocolError;
use crate::mcp::{CoreActionMeta, McpActionArgSpec, McpTool, McpToolCtx, McpToolError};
use crate::personality::MemorySnapshot;
use crate::{EntityOwnerRow, Principal, RemoveOwnerOutcome};

use super::get_memory::{
    GetMemoryOutput, format_authoring_personality, memory_class, payload_string, payload_tags,
    snapshot_payload_value,
};
use super::memory_spaces::{SpaceDefault, resolve_space_owner, space_key, space_label};
use super::{DESTRUCTIVE_NON_IDEMPOTENT, READ_ONLY, WRITE_NON_IDEMPOTENT};

const CORE_SHARE_SHARE_SCOPE_KEY: &str = "core_share:share";
const CORE_SHARE_UNSHARE_SCOPE_KEY: &str = "core_share:unshare";
const CORE_SHARE_PUBLISH_SCOPE_KEY: &str = "core_share:publish";
const CORE_SHARE_UNPUBLISH_SCOPE_KEY: &str = "core_share:unpublish";
const CORE_SHARE_LIST_SHARES_SCOPE_KEY: &str = "core_share:list_shares";
const CORE_SHARE_LIST_WORLD_SCOPE_KEY: &str = "core_share:list_world";
const DEFAULT_WORLD_LIMIT: u32 = 50;
const MAX_WORLD_LIMIT: u32 = 200;

pub const CORE_SHARE_ACTIONS: &[CoreActionMeta] = &[
    CoreActionMeta {
        tool: CoreShareTool::NAME,
        action: "share",
        scope_key: CORE_SHARE_SHARE_SCOPE_KEY,
        description: "Share one Memory or Goal entity with a space principal.",
        produces_schema_ids: &[],
        annotations: WRITE_NON_IDEMPOTENT,
    },
    CoreActionMeta {
        tool: CoreShareTool::NAME,
        action: "unshare",
        scope_key: CORE_SHARE_UNSHARE_SCOPE_KEY,
        description: "Remove one read-only entity share.",
        produces_schema_ids: &[],
        annotations: DESTRUCTIVE_NON_IDEMPOTENT,
    },
    CoreActionMeta {
        tool: CoreShareTool::NAME,
        action: "publish",
        scope_key: CORE_SHARE_PUBLISH_SCOPE_KEY,
        description: "Publish one Memory or Goal entity by adding the World read row.",
        produces_schema_ids: &[],
        annotations: WRITE_NON_IDEMPOTENT,
    },
    CoreActionMeta {
        tool: CoreShareTool::NAME,
        action: "unpublish",
        scope_key: CORE_SHARE_UNPUBLISH_SCOPE_KEY,
        description: "Remove the World read row from one entity.",
        produces_schema_ids: &[],
        annotations: DESTRUCTIVE_NON_IDEMPOTENT,
    },
    CoreActionMeta {
        tool: CoreShareTool::NAME,
        action: "list_shares",
        scope_key: CORE_SHARE_LIST_SHARES_SCOPE_KEY,
        description: "List home/share rows for one entity.",
        produces_schema_ids: &[],
        annotations: READ_ONLY,
    },
    CoreActionMeta {
        tool: CoreShareTool::NAME,
        action: "list_world",
        scope_key: CORE_SHARE_LIST_WORLD_SCOPE_KEY,
        description: "List World-readable memory entities.",
        produces_schema_ids: &[],
        annotations: READ_ONLY,
    },
];

#[derive(Debug, Default)]
pub struct CoreShareTool;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum CoreShareArgs {
    Share(ShareArgs),
    Unshare(UnshareArgs),
    Publish(PublishArgs),
    Unpublish(UnpublishArgs),
    ListShares(ListSharesArgs),
    ListWorld(ListWorldArgs),
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ShareArgs {
    /// Memory handle or id (`F`, `A`, `P`); `G` goal handles are accepted where present.
    pub entity: String,
    /// Space key from `core_memory_spaces`, e.g. `current`, `user:<uuid>`, or `group:<uuid>`.
    pub with: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UnshareArgs {
    /// Memory handle or id (`F`, `A`, `P`); `G` goal handles are accepted where present.
    pub entity: String,
    /// Space key from `core_memory_spaces`, e.g. `current`, `user:<uuid>`, or `group:<uuid>`.
    pub with: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PublishArgs {
    /// Memory handle or id (`F`, `A`, `P`); `G` goal handles are accepted where present.
    pub entity: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UnpublishArgs {
    /// Memory handle or id (`F`, `A`, `P`); `G` goal handles are accepted where present.
    pub entity: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListSharesArgs {
    /// Memory handle or id (`F`, `A`, `P`); `G` goal handles are accepted where present.
    pub entity: String,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ListWorldArgs {
    /// Maximum entities to return. Defaults to 50; clamped to 1..=200.
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum CoreShareOutput {
    Share(MutationOutput),
    Unshare(RemoveOwnerOutput),
    Publish(MutationOutput),
    Unpublish(RemoveOwnerOutput),
    ListShares(Vec<ShareOwnerOutput>),
    ListWorld(Vec<GetMemoryOutput>),
}

#[derive(Debug, Serialize)]
pub struct MutationOutput {
    pub ok: bool,
}

#[derive(Debug, Serialize)]
pub struct RemoveOwnerOutput {
    pub outcome: String,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct ShareOwnerOutput {
    pub owner: String,
    pub label: String,
    pub is_home: bool,
}

impl McpTool for CoreShareTool {
    const NAME: &'static str = "core_share";
    const DESCRIPTION: &'static str =
        "Sharing dispatcher — share/unshare/publish/unpublish/list_shares/list_world.";
    const ACTION_ARG_SPECS: &'static [McpActionArgSpec] = &[
        McpActionArgSpec {
            action: "share",
            allowed_fields: &["entity", "with"],
            required_fields: &["entity", "with"],
        },
        McpActionArgSpec {
            action: "unshare",
            allowed_fields: &["entity", "with"],
            required_fields: &["entity", "with"],
        },
        McpActionArgSpec {
            action: "publish",
            allowed_fields: &["entity"],
            required_fields: &["entity"],
        },
        McpActionArgSpec {
            action: "unpublish",
            allowed_fields: &["entity"],
            required_fields: &["entity"],
        },
        McpActionArgSpec {
            action: "list_shares",
            allowed_fields: &["entity"],
            required_fields: &["entity"],
        },
        McpActionArgSpec {
            action: "list_world",
            allowed_fields: &["limit"],
            required_fields: &[],
        },
    ];
    type Args = CoreShareArgs;
    type Output = CoreShareOutput;

    fn call(
        ctx: McpToolCtx,
        args: CoreShareArgs,
    ) -> BoxFuture<'static, Result<CoreShareOutput, McpToolError>> {
        Box::pin(async move {
            let engine = ctx
                .engine()
                .ok_or_else(|| McpToolError::Other("engine unavailable".into()))?;
            execute_share(engine, &ctx, args).await
        })
    }
}

trait ShareEngine: Sync {
    fn share_entry<'a>(
        &'a self,
        authz: &'a AuthzContext,
        entity: EntityId,
        with: Principal,
    ) -> BoxFuture<'a, Result<(), ProtocolError>>;

    fn unshare_entry<'a>(
        &'a self,
        authz: &'a AuthzContext,
        entity: EntityId,
        with: Principal,
    ) -> BoxFuture<'a, Result<RemoveOwnerOutcome, ProtocolError>>;

    fn list_entry_shares<'a>(
        &'a self,
        authz: &'a AuthzContext,
        entity: EntityId,
    ) -> BoxFuture<'a, Result<Vec<EntityOwnerRow>, ProtocolError>>;

    fn publish_entry<'a>(
        &'a self,
        authz: &'a AuthzContext,
        entity: EntityId,
    ) -> BoxFuture<'a, Result<(), ProtocolError>>;

    fn unpublish_entry<'a>(
        &'a self,
        authz: &'a AuthzContext,
        entity: EntityId,
    ) -> BoxFuture<'a, Result<RemoveOwnerOutcome, ProtocolError>>;

    fn list_world_entities<'a>(
        &'a self,
        authz: &'a AuthzContext,
        limit: usize,
    ) -> BoxFuture<'a, Result<Vec<MemorySnapshot>, ProtocolError>>;
}

impl ShareEngine for crate::Engine {
    fn share_entry<'a>(
        &'a self,
        authz: &'a AuthzContext,
        entity: EntityId,
        with: Principal,
    ) -> BoxFuture<'a, Result<(), ProtocolError>> {
        Box::pin(crate::Engine::share_entry(self, authz, entity, with))
    }

    fn unshare_entry<'a>(
        &'a self,
        authz: &'a AuthzContext,
        entity: EntityId,
        with: Principal,
    ) -> BoxFuture<'a, Result<RemoveOwnerOutcome, ProtocolError>> {
        Box::pin(crate::Engine::unshare_entry(self, authz, entity, with))
    }

    fn list_entry_shares<'a>(
        &'a self,
        authz: &'a AuthzContext,
        entity: EntityId,
    ) -> BoxFuture<'a, Result<Vec<EntityOwnerRow>, ProtocolError>> {
        Box::pin(crate::Engine::list_entry_shares(self, authz, entity))
    }

    fn publish_entry<'a>(
        &'a self,
        authz: &'a AuthzContext,
        entity: EntityId,
    ) -> BoxFuture<'a, Result<(), ProtocolError>> {
        Box::pin(crate::Engine::publish_entry(self, authz, entity))
    }

    fn unpublish_entry<'a>(
        &'a self,
        authz: &'a AuthzContext,
        entity: EntityId,
    ) -> BoxFuture<'a, Result<RemoveOwnerOutcome, ProtocolError>> {
        Box::pin(crate::Engine::unpublish_entry(self, authz, entity))
    }

    fn list_world_entities<'a>(
        &'a self,
        authz: &'a AuthzContext,
        limit: usize,
    ) -> BoxFuture<'a, Result<Vec<MemorySnapshot>, ProtocolError>> {
        Box::pin(crate::Engine::list_world_entities(self, authz, limit))
    }
}

async fn execute_share(
    engine: &dyn ShareEngine,
    ctx: &McpToolCtx,
    args: CoreShareArgs,
) -> Result<CoreShareOutput, McpToolError> {
    match args {
        CoreShareArgs::Share(args) => {
            let entity = resolve_entity(ctx, &args.entity)?;
            let with = resolve_principal(ctx, &args.with)?;
            engine.share_entry(&ctx.authz, entity, with).await?;
            Ok(CoreShareOutput::Share(MutationOutput { ok: true }))
        }
        CoreShareArgs::Unshare(args) => {
            let entity = resolve_entity(ctx, &args.entity)?;
            let with = resolve_principal(ctx, &args.with)?;
            let outcome = engine.unshare_entry(&ctx.authz, entity, with).await?;
            Ok(CoreShareOutput::Unshare(RemoveOwnerOutput {
                outcome: format_remove_owner_outcome(outcome).to_string(),
            }))
        }
        CoreShareArgs::Publish(args) => {
            let entity = resolve_entity(ctx, &args.entity)?;
            engine.publish_entry(&ctx.authz, entity).await?;
            Ok(CoreShareOutput::Publish(MutationOutput { ok: true }))
        }
        CoreShareArgs::Unpublish(args) => {
            let entity = resolve_entity(ctx, &args.entity)?;
            let outcome = engine.unpublish_entry(&ctx.authz, entity).await?;
            Ok(CoreShareOutput::Unpublish(RemoveOwnerOutput {
                outcome: format_remove_owner_outcome(outcome).to_string(),
            }))
        }
        CoreShareArgs::ListShares(args) => {
            let entity = resolve_entity(ctx, &args.entity)?;
            let shares = engine
                .list_entry_shares(&ctx.authz, entity)
                .await?
                .into_iter()
                .map(|row| format_share_owner(&row))
                .collect();
            Ok(CoreShareOutput::ListShares(shares))
        }
        CoreShareArgs::ListWorld(args) => {
            let limit = args
                .limit
                .unwrap_or(DEFAULT_WORLD_LIMIT)
                .clamp(1, MAX_WORLD_LIMIT);
            let entities = engine
                .list_world_entities(&ctx.authz, limit as usize)
                .await?
                .into_iter()
                .map(|snapshot| memory_output(ctx, snapshot, "world"))
                .collect::<Result<Vec<_>, McpToolError>>()?;
            Ok(CoreShareOutput::ListWorld(entities))
        }
    }
}

fn resolve_entity(ctx: &McpToolCtx, raw: &str) -> Result<EntityId, McpToolError> {
    match ctx.resolve_memory(raw) {
        Ok(memory_id) => Ok(EntityId::Memory(memory_id)),
        Err(memory_err) => ctx
            .resolve_goal(raw)
            .map(EntityId::Goal)
            .map_err(|_| memory_err),
    }
}

fn resolve_principal(ctx: &McpToolCtx, raw: &str) -> Result<Principal, McpToolError> {
    Ok(resolve_space_owner(ctx, Some(raw), SpaceDefault::Current)?.owner)
}

fn format_share_owner(row: &EntityOwnerRow) -> ShareOwnerOutput {
    ShareOwnerOutput {
        owner: space_key(&row.owner),
        label: space_label(&row.owner),
        is_home: row.is_home,
    }
}

fn format_remove_owner_outcome(outcome: RemoveOwnerOutcome) -> &'static str {
    match outcome {
        RemoveOwnerOutcome::Removed => "removed",
        RemoveOwnerOutcome::RefusedLastOwner => "refused_last_owner",
        RemoveOwnerOutcome::NotFound => "not_found",
    }
}

fn memory_output(
    ctx: &McpToolCtx,
    snapshot: MemorySnapshot,
    space: &str,
) -> Result<GetMemoryOutput, McpToolError> {
    let class = memory_class(&snapshot.kind)?;
    let handle = ctx.format_memory_with_class(snapshot.memory_id, class);
    let payload = snapshot_payload_value(snapshot.payload.as_ref())?;
    let title =
        payload_string(&payload, "title").or_else(|| payload_string(&payload, "conversation_id"));
    let body = payload_string(&payload, "body")
        .or_else(|| payload_string(&payload, "text"))
        .or_else(|| snapshot.text.clone());
    let tags = payload_tags(&payload);
    Ok(GetMemoryOutput {
        handle: handle.clone(),
        memory: handle,
        space: space.to_string(),
        kind: snapshot.kind,
        schema_id: snapshot.schema_id.as_str().to_string(),
        schema_version: snapshot.schema_version.into_inner(),
        authoring_personality_instance_id: format_authoring_personality(
            ctx,
            snapshot.authoring_personality_instance_id,
        ),
        text: snapshot.text,
        wake_chain_depth: snapshot.wake_chain_depth.into_inner(),
        payload,
        title,
        body,
        tags,
        neighbor_edges: None,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::{Arc, Mutex};

    use crate::access::world;
    use crate::mcp::{McpAuthorContext, McpToolExtensions, OutputMode};
    use crate::{
        AccessScope, AuthPath, AuthzContext, CapabilitySet, FlavorRegistry, Identity, MemoryId,
        SchemaId, SchemaVersion, ToolScope, UserId, WakeChainDepth,
    };

    use super::*;

    #[derive(Default)]
    struct MockShareEngine {
        published: Mutex<Vec<EntityId>>,
        world: Mutex<Vec<MemorySnapshot>>,
    }

    impl ShareEngine for MockShareEngine {
        fn share_entry<'a>(
            &'a self,
            _authz: &'a AuthzContext,
            _entity: EntityId,
            _with: Principal,
        ) -> BoxFuture<'a, Result<(), ProtocolError>> {
            Box::pin(async { Ok(()) })
        }

        fn unshare_entry<'a>(
            &'a self,
            _authz: &'a AuthzContext,
            _entity: EntityId,
            _with: Principal,
        ) -> BoxFuture<'a, Result<RemoveOwnerOutcome, ProtocolError>> {
            Box::pin(async { Ok(RemoveOwnerOutcome::Removed) })
        }

        fn list_entry_shares<'a>(
            &'a self,
            _authz: &'a AuthzContext,
            _entity: EntityId,
        ) -> BoxFuture<'a, Result<Vec<EntityOwnerRow>, ProtocolError>> {
            Box::pin(async { Ok(Vec::new()) })
        }

        fn publish_entry<'a>(
            &'a self,
            _authz: &'a AuthzContext,
            entity: EntityId,
        ) -> BoxFuture<'a, Result<(), ProtocolError>> {
            Box::pin(async move {
                self.published.lock().expect("published lock").push(entity);
                Ok(())
            })
        }

        fn unpublish_entry<'a>(
            &'a self,
            _authz: &'a AuthzContext,
            _entity: EntityId,
        ) -> BoxFuture<'a, Result<RemoveOwnerOutcome, ProtocolError>> {
            Box::pin(async { Ok(RemoveOwnerOutcome::Removed) })
        }

        fn list_world_entities<'a>(
            &'a self,
            _authz: &'a AuthzContext,
            _limit: usize,
        ) -> BoxFuture<'a, Result<Vec<MemorySnapshot>, ProtocolError>> {
            Box::pin(async move { Ok(self.world.lock().expect("world lock").clone()) })
        }
    }

    fn ctx() -> McpToolCtx {
        let owner = Principal::User(UserId::new(uuid::Uuid::now_v7()));
        let accessible_principals = HashSet::from([owner.clone(), world()]);
        McpToolCtx {
            owner: owner.clone(),
            authz: AuthzContext {
                identity: Identity {
                    principal: owner.clone(),
                    accessible_principals,
                    expires_at: None,
                    auth_epoch: 0,
                },
                capabilities: CapabilitySet {
                    tool_scope: ToolScope::All,
                    access: AccessScope::Unrestricted,
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
    async fn publish_routes_to_engine() {
        let ctx = ctx();
        let memory = MemoryId::new(uuid::Uuid::now_v7());
        let entity = ctx.format_fact_memory(memory);
        let engine = MockShareEngine::default();

        let out = execute_share(
            &engine,
            &ctx,
            CoreShareArgs::Publish(PublishArgs { entity }),
        )
        .await
        .expect("publish routes");

        assert!(matches!(
            out,
            CoreShareOutput::Publish(MutationOutput { ok: true })
        ));
        assert_eq!(
            engine.published.lock().expect("published lock").as_slice(),
            &[EntityId::Memory(memory)]
        );
    }

    #[tokio::test]
    async fn list_world_formats_snapshots_as_handles() {
        let ctx = ctx();
        let memory = MemoryId::new(uuid::Uuid::now_v7());
        let engine = MockShareEngine::default();
        engine
            .world
            .lock()
            .expect("world lock")
            .push(MemorySnapshot {
                memory_id: memory,
                kind: "Fact".into(),
                schema_id: SchemaId::new("core/test".into()),
                schema_version: SchemaVersion::new(1),
                authoring_personality_instance_id: None,
                text: Some("hello".into()),
                wake_chain_depth: WakeChainDepth::new(0),
                payload: None,
            });

        let out = execute_share(
            &engine,
            &ctx,
            CoreShareArgs::ListWorld(ListWorldArgs { limit: None }),
        )
        .await
        .expect("list world routes");

        let CoreShareOutput::ListWorld(entities) = out else {
            panic!("expected list_world output");
        };
        assert_eq!(entities.len(), 1);
        assert_eq!(entities[0].memory, ctx.format_fact_memory(memory));
        assert_eq!(entities[0].handle, entities[0].memory);
        assert_eq!(entities[0].space, "world");
        assert_eq!(entities[0].kind, "Fact");
    }

    #[test]
    fn remove_owner_outcome_uses_wire_strings() {
        assert_eq!(
            format_remove_owner_outcome(RemoveOwnerOutcome::RefusedLastOwner),
            "refused_last_owner"
        );
        assert_eq!(
            format_remove_owner_outcome(RemoveOwnerOutcome::NotFound),
            "not_found"
        );
    }
}
