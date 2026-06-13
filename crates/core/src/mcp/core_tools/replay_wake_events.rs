//! `core/replay_wake_events` — operator-triggered missed-wake replay.

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::Deserialize;
use uuid::Uuid;

use crate::ReplayWakeEventsRequest;
use crate::mcp::{McpTool, McpToolCtx, McpToolError};
use crate::personality::ReplayWakeEventsOutcome;

#[derive(Debug, Default)]
pub struct ReplayWakeEventsTool;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReplayWakeEventsArgs {
    /// `I`-handle for the personality whose active wake entries should
    /// be replayed against historical change events.
    pub personality: String,
    /// Optional `W`-handle. When omitted, all active wake entries on the
    /// personality are considered.
    pub wake_entry: Option<String>,
    /// Exclusive lower bound `change_event` seq.
    pub after_seq: Option<String>,
    /// Inclusive upper bound `change_event` seq.
    pub until_seq: Option<String>,
    /// Number of change events to scan. 0 or omitted uses the default.
    pub event_limit: Option<u16>,
    /// Maximum new invocations to start. 0 or omitted uses the default.
    pub max_invocations: Option<u16>,
}

impl McpTool for ReplayWakeEventsTool {
    const NAME: &'static str = "core/replay_wake_events";
    const DESCRIPTION: &'static str = "Replay missed eligible wake events for one personality \
         without moving its normal wake cursor. Existing invocation rows are not retried.";
    type Args = ReplayWakeEventsArgs;
    type Output = ReplayWakeEventsOutcome;

    fn call(
        ctx: McpToolCtx,
        args: ReplayWakeEventsArgs,
    ) -> BoxFuture<'static, Result<ReplayWakeEventsOutcome, McpToolError>> {
        Box::pin(async move {
            let personality_instance_id = ctx.resolve_personality(&args.personality)?;
            let wake_entry_id = args
                .wake_entry
                .as_deref()
                .map(|handle| ctx.resolve_wake_entry(handle))
                .transpose()?;
            let after_seq = parse_optional_uuid("after_seq", args.after_seq.as_deref())?;
            let until_seq = parse_optional_uuid("until_seq", args.until_seq.as_deref())?;
            let engine = ctx
                .engine()
                .ok_or_else(|| McpToolError::Other("engine unavailable".into()))?;

            engine
                .replay_missed_wakes(
                    &ctx.authz,
                    ReplayWakeEventsRequest {
                        principal: ctx.owner.principal.clone(),
                        org_id: None,
                        personality_instance_id,
                        wake_entry_id,
                        after_seq,
                        until_seq,
                        event_limit: args.event_limit.unwrap_or(0),
                        max_invocations: args.max_invocations.unwrap_or(0),
                    },
                )
                .await
                .map_err(|e| McpToolError::Other(e.to_string()))
        })
    }
}

fn parse_optional_uuid(field: &str, value: Option<&str>) -> Result<Option<Uuid>, McpToolError> {
    value
        .map(|s| {
            Uuid::parse_str(s).map_err(|e| McpToolError::InvalidInput(format!("{field}: {e}")))
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authz::{AuthPath, AuthzContext};
    use crate::mcp::HandleTable;
    use crate::mcp::OutputMode;
    use crate::verbs::query::MemoryStore;
    use crate::{Engine, FlavorRegistry, McpAuthorContext, OrgId, Owner, Principal, UserId};
    use std::sync::Arc;

    fn make_ctx() -> McpToolCtx {
        let owner = Owner {
            principal: Principal::User(UserId::new(uuid::Uuid::now_v7())),
            org_id: OrgId::new(uuid::Uuid::now_v7()),
        };
        let engine = Arc::new(Engine::new(
            FlavorRegistry::new().freeze(),
            MemoryStore::new(),
        ));
        McpToolCtx {
            pool: sqlx::PgPool::connect_lazy("postgres://x/x").expect("lazy"),
            owner: owner.clone(),
            authz: AuthzContext::single_owner(&owner, AuthPath::System),
            handles: Some(Arc::new(HandleTable::new())),
            mode: OutputMode::Handles,
            registry: Arc::new(FlavorRegistry::new().freeze()),
            author: McpAuthorContext {
                model_id: "t".into(),
                client_name: "t".into(),
                client_version: "0".into(),
                caller_self_perspective: None,
            },
            caller_self_perspective: None,
            master_token_id: None,
            engine: Some(engine),
        }
    }

    #[tokio::test]
    async fn replay_unknown_personality_handle_errs() {
        let err = ReplayWakeEventsTool::call(
            make_ctx(),
            ReplayWakeEventsArgs {
                personality: "I404".into(),
                wake_entry: None,
                after_seq: None,
                until_seq: None,
                event_limit: None,
                max_invocations: None,
            },
        )
        .await
        .expect_err("unknown handle");
        assert!(matches!(
            err,
            McpToolError::Resolve(crate::mcp::ResolveError::Unknown { .. })
        ));
    }
}
