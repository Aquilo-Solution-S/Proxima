//! `core/list_wake_invocations` — read-only wake runtime projection.

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ListWakeInvocationsRequest;
use crate::mcp::{McpTool, McpToolCtx, McpToolError};

#[derive(Debug, Default)]
pub struct ListWakeInvocationsTool;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListWakeInvocationsArgs {
    /// `I`-handle for the personality whose wake invocations to inspect.
    pub personality: String,
    /// Optional `W`-handle. Filters to one wake entry when present.
    pub wake_entry: Option<String>,
    /// Optional triggering memory handle, usually a chat-message `F...`
    /// handle. Filters through the ChangeEvent that caused the wake.
    pub triggering_memory: Option<String>,
    /// Optional raw ChangeEvent sequence UUID. Use when the trigger event
    /// is already known.
    pub change_event_seq: Option<String>,
    /// Maximum invocations to return. Omit or pass 0 for 20. Values above
    /// 100 are clamped by storage.
    pub limit: Option<u16>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ListWakeInvocationsOutput {
    pub invocations: Vec<WakeInvocationItem>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct WakeInvocationItem {
    pub invocation_id: String,
    pub personality: String,
    pub wake_entry: String,
    pub wake_entry_label: String,
    pub change_event_seq: String,
    pub status: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub turn_count: u16,
    pub cost_usd: f64,
    pub resolved_inference_target_ref: Option<String>,
    pub failure_reason: Option<String>,
    pub exit_code: Option<i32>,
    pub duration_ms: Option<u64>,
    pub stdout_tail: Option<String>,
    pub stderr_tail: Option<String>,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub continuation_intervention_decision: Option<String>,
    pub continuation_original_invocation_id: Option<String>,
    pub logs: Vec<WakeInvocationLogItem>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct WakeInvocationLogItem {
    pub log_seq: i64,
    pub at: String,
    pub phase: String,
    pub tool_id: Option<String>,
    pub status: String,
    pub duration_ms: Option<u64>,
    pub message_tail: Option<String>,
}

impl McpTool for ListWakeInvocationsTool {
    const NAME: &'static str = "core/list_wake_invocations";
    const DESCRIPTION: &'static str = "List read-only wake invocation runtime status and logs for \
         one personality. Args: `{\"personality\":\"I1\"}`. Optional filters: `wake_entry` (`W...`), \
         `triggering_memory` (`F...`, `A...`, or `P...` handle that caused the wake), \
         `change_event_seq` (raw ChangeEvent UUID), and `limit`.";
    type Args = ListWakeInvocationsArgs;
    type Output = ListWakeInvocationsOutput;

    fn call(
        ctx: McpToolCtx,
        args: ListWakeInvocationsArgs,
    ) -> BoxFuture<'static, Result<ListWakeInvocationsOutput, McpToolError>> {
        Box::pin(async move {
            let personality_instance_id = ctx.resolve_personality(&args.personality)?;
            let wake_entry_id = args
                .wake_entry
                .as_deref()
                .map(|handle| ctx.resolve_wake_entry(handle))
                .transpose()?;
            let triggering_memory_id = args
                .triggering_memory
                .as_deref()
                .map(|handle| ctx.resolve_memory(handle))
                .transpose()?;
            let change_event_seq = args
                .change_event_seq
                .as_deref()
                .map(|raw| {
                    uuid::Uuid::parse_str(raw)
                        .map_err(|e| McpToolError::InvalidInput(format!("change_event_seq: {e}")))
                })
                .transpose()?;
            let engine = ctx
                .engine()
                .ok_or_else(|| McpToolError::Other("engine unavailable".into()))?;
            let limit = match args.limit {
                Some(0) | None => 20,
                Some(limit) => limit,
            };
            let rows = engine
                .list_wake_invocations(ListWakeInvocationsRequest {
                    owner: ctx.owner.clone(),
                    personality_instance_id,
                    wake_entry_id,
                    triggering_memory_id,
                    change_event_seq,
                    limit,
                })
                .await
                .map_err(|e| McpToolError::Other(e.to_string()))?;

            let invocations = rows
                .into_iter()
                .map(|row| WakeInvocationItem {
                    invocation_id: row.invocation_id.to_string(),
                    personality: ctx.format_personality(row.personality_instance_id),
                    wake_entry: ctx.format_wake_entry(row.wake_entry_id),
                    wake_entry_label: row.wake_entry_label,
                    change_event_seq: row.change_event_seq.to_string(),
                    status: row.status.as_str().to_string(),
                    started_at: row.started_at.to_string(),
                    finished_at: row.finished_at.map(|v| v.to_string()),
                    turn_count: row.turn_count,
                    cost_usd: row.cost_usd,
                    resolved_inference_target_ref: row.resolved_inference_target_ref,
                    failure_reason: row.failure_reason,
                    exit_code: row.exit_code,
                    duration_ms: row.duration_ms,
                    stdout_tail: row.stdout_tail,
                    stderr_tail: row.stderr_tail,
                    stdout_truncated: row.stdout_truncated,
                    stderr_truncated: row.stderr_truncated,
                    continuation_intervention_decision: row
                        .continuation_intervention_decision_memory_id
                        .map(|id| ctx.format_fact_memory(crate::MemoryId::new(id))),
                    continuation_original_invocation_id: row
                        .continuation_original_invocation_id
                        .map(|id| id.to_string()),
                    logs: row
                        .logs
                        .into_iter()
                        .map(|log| WakeInvocationLogItem {
                            log_seq: log.log_seq,
                            at: log.at.to_string(),
                            phase: log.phase,
                            tool_id: log.tool_id,
                            status: log.status.as_str().to_string(),
                            duration_ms: log.duration_ms,
                            message_tail: log.message_tail,
                        })
                        .collect(),
                })
                .collect();
            Ok(ListWakeInvocationsOutput { invocations })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::NoAuth;
    use crate::mcp::{HandleTable, OutputMode};
    use crate::verbs::query::MemoryStore;
    use crate::{Engine, FlavorRegistry, McpAuthorContext, OrgId, Owner, Principal, UserId};
    use std::sync::Arc;

    #[tokio::test]
    async fn list_wake_invocations_unknown_personality_handle_errs() {
        let owner = Owner {
            principal: Principal::User(UserId::new(uuid::Uuid::now_v7())),
            org_id: OrgId::new(uuid::Uuid::now_v7()),
        };
        let resolver = NoAuth::new(owner.principal.clone(), owner.clone());
        let engine = Arc::new(Engine::new(
            FlavorRegistry::new().freeze(),
            MemoryStore::new(),
            Box::new(resolver),
        ));
        let ctx = McpToolCtx {
            pool: sqlx::PgPool::connect_lazy("postgres://x/x").expect("lazy"),
            owner,
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
        };
        let err = ListWakeInvocationsTool::call(
            ctx,
            ListWakeInvocationsArgs {
                personality: "I404".into(),
                wake_entry: None,
                triggering_memory: None,
                change_event_seq: None,
                limit: None,
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
