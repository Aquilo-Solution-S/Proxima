use std::sync::Arc;

use async_trait::async_trait;
use futures::future::BoxFuture;

use crate::AccessKind;

use super::{
    McpToolCtx, McpToolError, core_action_meta, core_tool_annotations, core_tool_has_actions,
};

#[derive(Debug)]
pub struct ToolCall {
    pub name: String,
    pub args: serde_json::Value,
    pub ctx: McpToolCtx,
}

pub type TerminalDispatch<'a> =
    Box<dyn FnOnce(ToolCall) -> BoxFuture<'a, Result<serde_json::Value, McpToolError>> + Send + 'a>;

pub struct Next<'a> {
    rest: &'a [Arc<dyn RequestBehavior>],
    terminal: Option<TerminalDispatch<'a>>,
}

impl std::fmt::Debug for Next<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Next")
            .field("rest_len", &self.rest.len())
            .field("has_terminal", &self.terminal.is_some())
            .finish_non_exhaustive()
    }
}

impl<'a> Next<'a> {
    #[must_use]
    pub fn new(rest: &'a [Arc<dyn RequestBehavior>], terminal: TerminalDispatch<'a>) -> Self {
        Self {
            rest,
            terminal: Some(terminal),
        }
    }

    /// # Errors
    ///
    /// Returns a behavior or terminal tool error.
    pub async fn run(mut self, call: ToolCall) -> Result<serde_json::Value, McpToolError> {
        if let Some((head, tail)) = self.rest.split_first() {
            head.handle(
                call,
                Self {
                    rest: tail,
                    terminal: self.terminal.take(),
                },
            )
            .await
        } else {
            let Some(terminal) = self.terminal.take() else {
                return Err(McpToolError::Other("terminal dispatch missing".into()));
            };
            terminal(call).await
        }
    }
}

#[async_trait]
pub trait RequestBehavior: Send + Sync + std::fmt::Debug {
    async fn handle(
        &self,
        call: ToolCall,
        next: Next<'_>,
    ) -> Result<serde_json::Value, McpToolError>;
}

#[derive(Debug, Default)]
pub struct ScopeGateBehavior;

impl ScopeGateBehavior {
    fn enforce_scope(
        tool: &str,
        args: &serde_json::Value,
        ctx: &McpToolCtx,
    ) -> Result<(), McpToolError> {
        let scope = ctx.authz.tool_scope();
        if core_tool_has_actions(tool) {
            if !scope.allows_group_advertisement(tool) {
                return Err(McpToolError::NotAuthorized(tool.to_string()));
            }
            let Some(action) = args.get("action").and_then(serde_json::Value::as_str) else {
                return Err(McpToolError::InvalidInput(format!(
                    "tool {tool} requires string action"
                )));
            };
            if core_action_meta(tool, action).is_none() {
                return Err(McpToolError::InvalidInput(format!(
                    "unknown action {action:?} for tool {tool}"
                )));
            }
            if !scope.allows_action(tool, action) {
                return Err(McpToolError::NotAuthorized(format!("{tool}:{action}")));
            }
        } else if !scope.allows(tool) {
            return Err(McpToolError::NotAuthorized(tool.to_string()));
        }
        Self::enforce_owner_role(tool, args, ctx)?;
        Ok(())
    }

    fn enforce_owner_role(
        tool: &str,
        args: &serde_json::Value,
        ctx: &McpToolCtx,
    ) -> Result<(), McpToolError> {
        let read_only = args
            .get("action")
            .and_then(serde_json::Value::as_str)
            .and_then(|action| core_action_meta(tool, action))
            .map(|meta| meta.annotations)
            .or_else(|| core_tool_annotations(tool))
            .and_then(|annotations| annotations.read_only)
            .unwrap_or(false);
        let allowed = if read_only {
            ctx.authz.may_read(&ctx.owner, AccessKind::Fact)
        } else {
            ctx.authz.may_write(&ctx.owner, AccessKind::Fact)
        };
        if allowed {
            Ok(())
        } else {
            Err(McpToolError::NotAuthorized(tool.to_string()))
        }
    }
}

#[async_trait]
impl RequestBehavior for ScopeGateBehavior {
    async fn handle(
        &self,
        call: ToolCall,
        next: Next<'_>,
    ) -> Result<serde_json::Value, McpToolError> {
        Self::enforce_scope(&call.name, &call.args, &call.ctx)?;
        next.run(call).await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::protocol::tool as protocol_tool;
    use crate::{
        AuthPath, AuthzContext, FlavorRegistry, McpAuthorContext, McpToolExtensions, OutputMode,
        OwnerRef, ToolScope, UserId,
    };

    #[derive(Debug)]
    struct RecordingBehavior {
        label: &'static str,
        calls: Arc<Mutex<Vec<&'static str>>>,
    }

    #[async_trait]
    impl RequestBehavior for RecordingBehavior {
        async fn handle(
            &self,
            call: ToolCall,
            next: Next<'_>,
        ) -> Result<serde_json::Value, McpToolError> {
            self.calls.lock().expect("recording lock").push(self.label);
            next.run(call).await
        }
    }

    #[tokio::test]
    async fn chain_runs_scope_gate_then_flavor_behavior_then_terminal() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let behaviors: Vec<Arc<dyn RequestBehavior>> = vec![
            Arc::new(ScopeGateBehavior),
            Arc::new(RecordingBehavior {
                label: "flavor",
                calls: calls.clone(),
            }),
        ];
        let terminal_calls = calls.clone();
        let terminal: TerminalDispatch<'_> = Box::new(move |call| {
            Box::pin(async move {
                terminal_calls
                    .lock()
                    .expect("recording lock")
                    .push("terminal");
                Ok(serde_json::json!({
                    "tool": call.name,
                    "args": call.args,
                }))
            })
        });

        let output = Next::new(&behaviors, terminal)
            .run(ToolCall {
                name: protocol_tool::CORE_SEARCH_MEMORIES.to_string(),
                args: serde_json::json!({ "query": "x" }),
                ctx: test_ctx(ToolScope::All),
            })
            .await
            .expect("chain output");

        assert_eq!(output["tool"], protocol_tool::CORE_SEARCH_MEMORIES);
        assert_eq!(
            calls.lock().expect("recording lock").as_slice(),
            ["flavor", "terminal"]
        );
    }

    #[tokio::test]
    async fn scope_gate_denies_out_of_palette_before_terminal() {
        let behaviors: Vec<Arc<dyn RequestBehavior>> = vec![Arc::new(ScopeGateBehavior)];
        let terminal: TerminalDispatch<'_> =
            Box::new(|_call| Box::pin(async { Ok(serde_json::json!({ "unexpected": true })) }));

        let err = Next::new(&behaviors, terminal)
            .run(ToolCall {
                name: protocol_tool::CORE_SEARCH_MEMORIES.to_string(),
                args: serde_json::json!({ "query": "x" }),
                ctx: test_ctx(ToolScope::Palette(Vec::new())),
            })
            .await
            .expect_err("scope denial");

        assert!(
            matches!(err, McpToolError::NotAuthorized(ref tool) if tool == protocol_tool::CORE_SEARCH_MEMORIES)
        );
        assert_eq!(err.kind(), super::super::McpToolErrorKind::InvalidRequest);
    }

    #[tokio::test]
    async fn scope_gate_requires_exact_palette_match_for_flat_tools() {
        let action_only = ScopeGateBehavior::enforce_scope(
            protocol_tool::CORE_REMEMBER,
            &serde_json::json!({ "title": "t", "body": "b" }),
            &test_ctx(ToolScope::Palette(vec![format!(
                "{}:x",
                protocol_tool::CORE_REMEMBER
            )])),
        )
        .expect_err("flat tool requires exact palette entry");

        assert!(
            matches!(action_only, McpToolError::NotAuthorized(ref tool) if tool == protocol_tool::CORE_REMEMBER)
        );

        ScopeGateBehavior::enforce_scope(
            protocol_tool::CORE_REMEMBER,
            &serde_json::json!({ "title": "t", "body": "b" }),
            &test_ctx(ToolScope::Palette(vec![
                protocol_tool::CORE_REMEMBER.to_string(),
            ])),
        )
        .expect("bare palette entry allows flat tool");
    }

    #[test]
    fn owner_role_gate_allows_read_tools_and_rejects_write_tools_for_viewer() {
        let subject = UserId::new(uuid::Uuid::now_v7());
        let group = OwnerRef::Group(crate::GroupId::new(uuid::Uuid::now_v7()));
        let authz = AuthzContext::for_subject_with_role(
            subject,
            [(group, crate::Role::viewer())],
            AuthPath::HostBearer,
        )
        .narrowed_to_owner(group)
        .expect("viewer can bind readable group")
        .with_tool_scope(ToolScope::All);
        let ctx = test_ctx_with_authz(group, authz);

        ScopeGateBehavior::enforce_scope(
            protocol_tool::CORE_SEARCH_MEMORIES,
            &serde_json::json!({ "query": "x" }),
            &ctx,
        )
        .expect("viewer can call read-only search");

        let err = ScopeGateBehavior::enforce_scope(
            protocol_tool::CORE_REMEMBER,
            &serde_json::json!({ "title": "t", "body": "b" }),
            &ctx,
        )
        .expect_err("viewer cannot call write tool");
        assert!(
            matches!(err, McpToolError::NotAuthorized(ref tool) if tool == protocol_tool::CORE_REMEMBER)
        );
    }

    fn test_ctx(tool_scope: ToolScope) -> McpToolCtx {
        let owner = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
        let authz =
            AuthzContext::single_owner(&owner, AuthPath::HostBearer).with_tool_scope(tool_scope);
        test_ctx_with_authz(owner, authz)
    }

    fn test_ctx_with_authz(owner: OwnerRef, authz: AuthzContext) -> McpToolCtx {
        McpToolCtx {
            owner,
            authz,
            handles: None,
            mode: OutputMode::PrefixedIds,
            registry: Arc::new(FlavorRegistry::new().freeze_or_panic_for_tests()),
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
}
