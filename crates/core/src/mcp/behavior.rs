use std::sync::Arc;

use async_trait::async_trait;
use futures::future::BoxFuture;

use crate::AccessKind;

use super::{McpActionArgSpec, McpToolCtx, McpToolDescriptor, McpToolError, core_tool_annotations};

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
        // Whether a tool dispatches actions is read off its descriptor, not
        // off the substrate `CoreActionMeta` tables: a flavor dispatcher is
        // absent from those tables, so keying on them would drop it to the
        // whole-tool branch below — a palette holding only
        // `flavor_tool:action` would deny the tool outright, and an unknown
        // action would sail past this gate into the tool.
        let descriptor = ctx.registry.mcp_tool(tool);
        let specs = descriptor.map_or(&[] as &[McpActionArgSpec], |d| d.action_arg_specs);
        let argv_specs =
            descriptor.map_or(&[] as &[super::McpArgvActionSpec], |d| d.argv_action_specs);
        if !specs.is_empty() {
            if !scope.allows_group_advertisement(tool) {
                return Err(McpToolError::NotAuthorized(tool.to_string()));
            }
            let Some(action) = args.get("action").and_then(serde_json::Value::as_str) else {
                return Err(McpToolError::InvalidInput(format!(
                    "tool {tool} requires string action"
                )));
            };
            if !specs.iter().any(|spec| spec.action == action) {
                return Err(McpToolError::InvalidInput(format!(
                    "unknown action {action:?} for tool {tool}"
                )));
            }
            if !scope.allows_action(tool, action) {
                return Err(McpToolError::NotAuthorized(format!("{tool}:{action}")));
            }
        } else if !argv_specs.is_empty() {
            if !scope.allows_group_advertisement(tool) {
                return Err(McpToolError::NotAuthorized(tool.to_string()));
            }
            // The same resolution the terminal dispatch runs, so the gate
            // and the call share one vocabulary: the derived key is what
            // `allows_action` judges, and argv matching no declared prefix
            // is refused here rather than sailing past into the tool.
            let action = super::resolve_argv_action(tool, argv_specs, args)?;
            if !scope.allows_action(tool, action) {
                return Err(McpToolError::NotAuthorized(format!("{tool}:{action}")));
            }
        } else if !scope.allows(tool) {
            return Err(McpToolError::NotAuthorized(tool.to_string()));
        }
        Self::enforce_owner_role(tool, args, descriptor, ctx)?;
        Ok(())
    }

    fn enforce_owner_role(
        tool: &str,
        args: &serde_json::Value,
        descriptor: Option<&McpToolDescriptor>,
        ctx: &McpToolCtx,
    ) -> Result<(), McpToolError> {
        // Resource reads are reads. `read_resource` funnels through this same
        // gate with the resource's scope key in `tool`, and nothing in the
        // tool manifest is keyed by a `resource:` name — so the lookups below
        // all miss and the `unwrap_or(false)` default would ask for *write*
        // access, denying a read-only role every `proxima://` resource
        // `resources/list` advertises to it.
        //
        // The answer comes from the declaration, not from the shape of the
        // string: an unknown `resource:`-prefixed key is not waved through
        // as a read on the strength of its prefix.
        let read_only = if let Some(resource) = crate::flavor::FLAVOR_0.resource_by_scope_key(tool)
        {
            resource.read_only
        } else if let Some(descriptor) =
            descriptor.filter(|value| !value.action_arg_specs.is_empty())
        {
            // The action spec is the per-action behaviour authority for
            // substrate and flavor dispatchers alike. It deliberately does
            // not inherit the parent tool's declaration: silence is a write,
            // not permission to turn a later mutation into a viewer-callable
            // action.
            let action = args
                .get("action")
                .and_then(serde_json::Value::as_str)
                .expect("dispatcher action was validated before owner-role enforcement");
            descriptor
                .resolved_action_annotations(action)
                .and_then(|annotations| annotations.read_only)
                .unwrap_or(false)
        } else {
            // Flat tools still resolve their own declaration, then the
            // substrate manifest.
            descriptor
                .and_then(McpToolDescriptor::resolved_annotations)
                .or_else(|| core_tool_annotations(tool))
                .and_then(|annotations| annotations.read_only)
                .unwrap_or(false)
        };
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
        AuthPath, AuthzContext, FlavorRegistry, FlavorServices, McpAuthorContext, OwnerRef,
        ToolScope, UserId,
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

        ScopeGateBehavior::enforce_scope(
            protocol_tool::CORE_SEARCH_MEMORIES,
            &serde_json::json!({ "action": "unexpected", "query": "x" }),
            &ctx,
        )
        .expect("an unexpected field cannot reclassify a flat read as a dispatcher write");

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

    /// A viewer on a group owner: may read, may not write.
    fn read_only_ctx() -> McpToolCtx {
        let subject = UserId::new(uuid::Uuid::now_v7());
        let owner = OwnerRef::Group(crate::GroupId::new(uuid::Uuid::now_v7()));
        let authz = AuthzContext::for_subject_with_role(
            subject,
            [(owner, crate::access::Role::viewer())],
            AuthPath::HostBearer,
        )
        .with_tool_scope(ToolScope::All);
        assert!(authz.may_read(&owner, AccessKind::Fact), "viewer reads");
        assert!(
            !authz.may_write(&owner, AccessKind::Fact),
            "viewer does not write"
        );
        test_ctx_with_authz(owner, authz)
    }

    /// `read_resource` runs through this same gate with the resource's
    /// scope key as the tool name. Nothing in the tool manifest is keyed
    /// by a `resource:` name; without a read exemption, lookup misses and
    /// the default demands WRITE.
    #[tokio::test]
    async fn a_viewer_may_read_resources() {
        for scope_key in [
            crate::protocol::resource::SCHEMAS,
            crate::protocol::resource::MEMORIES,
            crate::protocol::resource::GRAPH,
        ] {
            let behaviors: Vec<Arc<dyn RequestBehavior>> = vec![Arc::new(ScopeGateBehavior)];
            let terminal: TerminalDispatch<'_> =
                Box::new(|_call| Box::pin(async { Ok(serde_json::json!({"ok": true})) }));
            let out = Next::new(&behaviors, terminal)
                .run(ToolCall {
                    name: scope_key.to_string(),
                    args: serde_json::json!({ "uri": "proxima://schemas" }),
                    ctx: read_only_ctx(),
                })
                .await;
            assert!(out.is_ok(), "{scope_key} must be readable by a viewer");
        }
    }

    /// The read exemption is keyed on the `resource:` namespace, so it must
    /// not hand a viewer a writing tool.
    #[tokio::test]
    async fn a_viewer_still_may_not_write() {
        let behaviors: Vec<Arc<dyn RequestBehavior>> = vec![Arc::new(ScopeGateBehavior)];
        let terminal: TerminalDispatch<'_> =
            Box::new(|_call| Box::pin(async { Ok(serde_json::json!({"ok": true})) }));
        let out = Next::new(&behaviors, terminal)
            .run(ToolCall {
                name: protocol_tool::CORE_REMEMBER.to_string(),
                args: serde_json::json!({ "text": "x" }),
                ctx: read_only_ctx(),
            })
            .await;
        assert!(out.is_err(), "core_remember must stay denied for a viewer");
    }

    fn test_ctx_with_authz(owner: OwnerRef, authz: AuthzContext) -> McpToolCtx {
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
mod argv_scope_tests {
    use std::sync::Arc;

    use futures::future::BoxFuture;

    use super::ScopeGateBehavior;
    use crate::mcp::{
        McpArgvActionSpec, McpAuthorContext, McpTool, McpToolAnnotations, McpToolAudience,
        McpToolCtx, McpToolError, McpToolErrorKind,
    };
    use crate::{
        AuthPath, AuthzContext, FlavorRegistry, FlavorServices, OwnerRef, ToolScope, UserId,
    };

    #[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
    struct ArgvArgs {
        #[schemars(description = "command words followed by flags")]
        _argv: Vec<String>,
    }

    /// An argv-keyed dispatcher with two commands sharing a first word, so
    /// the gate's derivation has to pick by longest prefix.
    struct ArgvTool;

    impl McpTool for ArgvTool {
        const NAME: &'static str = "proxima-stub_cli";
        const DESCRIPTION: &'static str = "An argv-keyed fixture dispatcher.";
        const ARGV_ACTION_SPECS: &'static [McpArgvActionSpec] = &[
            McpArgvActionSpec {
                action: "approval",
                argv_prefix: &["approval"],
                audience: McpToolAudience::Shared,
            },
            McpArgvActionSpec {
                action: "approval-decide",
                argv_prefix: &["approval", "decide"],
                audience: McpToolAudience::Shared,
            },
        ];
        const ANNOTATIONS: Option<McpToolAnnotations> =
            Some(McpToolAnnotations::new().read_only(false).open_world(false));
        type Args = ArgvArgs;
        type Output = ();
        fn call(_: McpToolCtx, _: Self::Args) -> BoxFuture<'static, Result<(), McpToolError>> {
            Box::pin(async { Ok(()) })
        }
    }

    fn argv_ctx(tool_scope: ToolScope) -> McpToolCtx {
        let owner = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
        let mut registry = FlavorRegistry::new();
        registry.add_mcp_tool_or_panic_for_tests::<ArgvTool>("proxima-stub");
        McpToolCtx {
            owner,
            authz: AuthzContext::single_owner(&owner, AuthPath::HostBearer)
                .with_tool_scope(tool_scope),
            registry: Arc::new(registry.freeze_or_panic_for_tests()),
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

    /// The gate judges the DERIVED key, so `tools/list` (which advertises
    /// leaves from the same specs) and `tools/call` share one vocabulary: a
    /// palette holding the leaf admits the call, and the same palette
    /// without it denies the same argv.
    #[test]
    fn enforce_scope_gates_the_derived_argv_action() {
        let args = serde_json::json!({ "argv": ["approval", "decide", "--id", "7"] });

        // Positive control: the leaf in the palette admits the call.
        ScopeGateBehavior::enforce_scope(
            ArgvTool::NAME,
            &args,
            &argv_ctx(ToolScope::Palette(vec![format!(
                "{}:approval-decide",
                ArgvTool::NAME
            )])),
        )
        .expect("a palette holding the derived leaf admits the call");

        // A palette holding only the SIBLING command's leaf denies this
        // one — proof the gate derived `approval-decide` by longest prefix
        // rather than settling for the one-word match.
        let err = ScopeGateBehavior::enforce_scope(
            ArgvTool::NAME,
            &args,
            &argv_ctx(ToolScope::Palette(vec![format!(
                "{}:approval",
                ArgvTool::NAME
            )])),
        )
        .expect_err("the sibling leaf must not admit the longer command");
        assert!(
            matches!(err, McpToolError::NotAuthorized(ref key)
                if key == &format!("{}:approval-decide", ArgvTool::NAME)),
            "got {err:?}",
        );
    }

    /// The closed set holds at the gate too: argv outside the declared
    /// commands is a validation error, never a pass-through to the tool.
    #[test]
    fn enforce_scope_refuses_argv_outside_the_vocabulary() {
        let err = ScopeGateBehavior::enforce_scope(
            ArgvTool::NAME,
            &serde_json::json!({ "argv": ["unknown", "verb"] }),
            &argv_ctx(ToolScope::All),
        )
        .expect_err("unmatched argv is refused");
        assert_eq!(err.kind(), McpToolErrorKind::InvalidInput);

        // Positive control: the same scope serves a declared command.
        ScopeGateBehavior::enforce_scope(
            ArgvTool::NAME,
            &serde_json::json!({ "argv": ["approval"] }),
            &argv_ctx(ToolScope::All),
        )
        .expect("a declared command passes under All");
    }
}

#[cfg(test)]
mod owner_role_tests {
    use super::ScopeGateBehavior;
    use crate::access::Role;
    use crate::mcp::{McpAuthorContext, McpTool, McpToolAnnotations, McpToolCtx, McpToolError};
    use crate::{
        AuthPath, AuthzContext, FlavorRegistry, FlavorServices, GroupId, OwnerRef, UserId,
    };
    use futures::future::BoxFuture;
    use std::sync::Arc;

    #[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
    struct StubArgs {
        #[schemars(description = "anything")]
        _query: String,
    }

    /// A flavor tool that says it only reads.
    #[derive(Debug)]
    struct DeclaredReadTool;

    impl McpTool for DeclaredReadTool {
        const NAME: &'static str = "proxima-stub_search";
        const DESCRIPTION: &'static str = "A flavor read tool that declares itself read-only.";
        const ANNOTATIONS: Option<McpToolAnnotations> =
            Some(McpToolAnnotations::new().read_only(true).open_world(false));
        type Args = StubArgs;
        type Output = ();
        fn call(_: McpToolCtx, _: Self::Args) -> BoxFuture<'static, Result<(), McpToolError>> {
            Box::pin(async { Ok(()) })
        }
    }

    /// A flavor tool that says nothing. `try_freeze` refuses to seal a
    /// registry containing one, so this never reaches a served registry —
    /// see `crate::flavor::freeze`. It stays here to pin that refusal.
    #[derive(Debug)]
    struct SilentTool;

    impl McpTool for SilentTool {
        const NAME: &'static str = "proxima-stub_silent";
        const DESCRIPTION: &'static str = "A flavor tool that declares no annotations.";
        type Args = StubArgs;
        type Output = ();
        fn call(_: McpToolCtx, _: Self::Args) -> BoxFuture<'static, Result<(), McpToolError>> {
            Box::pin(async { Ok(()) })
        }
    }

    /// A group viewer: read on everything, write on nothing.
    fn viewer_ctx(tool_owner: OwnerRef) -> McpToolCtx {
        let subject = UserId::new(uuid::Uuid::now_v7());
        let mut registry = FlavorRegistry::new();
        registry.add_mcp_tool_or_panic_for_tests::<DeclaredReadTool>("proxima-stub");
        McpToolCtx {
            owner: tool_owner,
            authz: AuthzContext::for_subject_with_role(
                subject,
                [(tool_owner, Role::viewer())],
                AuthPath::HostBearer,
            ),
            registry: Arc::new(registry.freeze_or_panic_for_tests()),
            author: McpAuthorContext {
                model_id: "t".into(),
                client_name: "t".into(),
                client_version: "0".into(),
                caller_self_perspective: None,
            },
            caller_self_perspective: None,
            services: FlavorServices::default(),
            engine: None,
        }
    }

    /// A read-only role can call a flavor tool that declares itself read-only.
    /// `enforce_owner_role` reads the descriptor's annotations; a missing
    /// declaration is treated as WRITE.
    #[test]
    fn a_viewer_may_call_a_flavor_tool_that_declares_itself_read_only() {
        let owner = OwnerRef::Group(GroupId::new(uuid::Uuid::now_v7()));
        let ctx = viewer_ctx(owner);
        let args = serde_json::json!({"query": "anything"});

        assert!(
            ScopeGateBehavior::enforce_owner_role(
                "core_search_memories",
                &args,
                ctx.registry.mcp_tool("core_search_memories"),
                &ctx,
            )
            .is_ok(),
            "core's read tool is annotated read-only, so a viewer passes"
        );
        assert!(
            ScopeGateBehavior::enforce_owner_role(
                DeclaredReadTool::NAME,
                &args,
                ctx.registry.mcp_tool(DeclaredReadTool::NAME),
                &ctx,
            )
            .is_ok(),
            "a flavor read tool that declares read_only must not be billed as a write"
        );
    }

    /// A registry containing a tool that declares nothing does not seal.
    ///
    /// This is the primary defence, and it fires at boot rather than at the
    /// first refused call. The gate's `unwrap_or(false)` below is the second
    /// one, for a name that never reached the registry at all.
    #[test]
    fn a_flavor_tool_that_declares_nothing_cannot_be_frozen() {
        let mut registry = FlavorRegistry::new();
        registry.add_mcp_tool_or_panic_for_tests::<DeclaredReadTool>("proxima-stub");
        registry.add_mcp_tool_or_panic_for_tests::<SilentTool>("proxima-stub");
        let err = registry.try_freeze().expect_err("silence must not seal");
        assert!(
            matches!(
                err,
                crate::FlavorRegistryError::UndeclaredToolBehavior {
                    name: "proxima-stub_silent"
                }
            ),
            "got {err:?}",
        );
        // The message names `ANNOTATIONS`; a silent tool is otherwise
        // billed as a write with no stated cause.
        let rendered = err.to_string();
        assert!(rendered.contains("ANNOTATIONS"), "{rendered}");
    }

    /// Declaring nothing still means write. The default has to stay
    /// conservative: a tool that has not thought about it may well write,
    /// and guessing "read" would hand a viewer a mutation.
    ///
    /// Freeze now rejects a *registered* silent tool, so the reachable case
    /// is a name the registry never saw — a tool dispatched by a name that
    /// does not match any descriptor.
    #[test]
    fn a_tool_the_registry_never_saw_is_still_a_write() {
        let owner = OwnerRef::Group(GroupId::new(uuid::Uuid::now_v7()));
        let ctx = viewer_ctx(owner);
        let args = serde_json::json!({"query": "anything"});
        assert!(
            ScopeGateBehavior::enforce_owner_role(
                SilentTool::NAME,
                &args,
                ctx.registry.mcp_tool(SilentTool::NAME),
                &ctx,
            )
            .is_err(),
            "silence must not be read as a promise to only read"
        );
    }
}
