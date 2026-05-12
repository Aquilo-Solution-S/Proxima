# Task 4.2 — Substrate dispatch bridge

> Part of [Proxima Harness Implementation Plan](README.md). Subagent execution: implement steps in order, commit at the end of the task.

**Why this task is shaped the way it is.** The live MCP tool surface is not only `FlavorRegistryFrozen::list_mcp_tools()`. Wake-authenticated calls also expose the substrate personality pack through `DevMcpServer::substrate_tools()` and dispatch them through `DevMcpServer::call_personality_tool` (`core/fetch_memory`, `core/emit_abstraction`, `core/emit_perspective`, `core/emit_goal`, `core/create_edge`, etc.). Those tools need `WakeTokenContext`, read-log propagation, `writeable_schemas_for_palette`, `writeable_relations_for_palette`, and wake-invocation logging. Calling only `McpToolDescriptor.call` would hide or reject existing Code Engineer palettes.

The harness must therefore receive a **substrate bridge**, not a raw `McpToolCtx` factory. The bridge is core-owned, implemented by `DevMcpServer`, and exposes the same combined inventory and call semantics as the HTTP MCP path while staying in-process: no HTTP, no JSON-RPC.

**Files:**
- Modify: `crates/core/src/mcp/mod.rs` (add the bridge trait + DTOs)
- Modify: `crates/mcp-server/src/server.rs` (impl the trait for `DevMcpServer`)
- Create: `crates/harness/src/tools/substrate_dispatch.rs`

- [ ] **Step 1: Add `HarnessSubstrateBridge` to `proxima-core::mcp`**

Open `crates/core/src/mcp/mod.rs`. Near the existing `McpAuthorContext` definitions, add:

```rust
use async_trait::async_trait;

#[derive(Debug, Clone)]
pub struct HarnessSubstrateToolSpec {
    pub canonical_name: String,
    pub description: String,
    pub args_schema: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct HarnessSubstrateCall {
    pub canonical_name: String,
    pub args: serde_json::Value,
    pub owner: Owner,
    /// Wake token minted by `fire_wake_entry`. The bridge resolves this
    /// through the existing `WakeTokenStore` and reconstructs the same
    /// wake-scoped auth context the HTTP MCP layer would have used.
    pub wake_token: uuid::Uuid,
    pub author: McpAuthorContext,
}

#[derive(Debug, thiserror::Error)]
pub enum HarnessSubstrateError {
    #[error("tool not found: {0}")]
    ToolNotFound(String),
    #[error("tool not authorized for wake palette: {0}")]
    Unauthorized(String),
    #[error("wake token not found or expired")]
    MissingWakeContext,
    #[error("storage: {0}")]
    Storage(String),
    #[error("layering: {0}")]
    Layering(String),
    #[error("tool: {0}")]
    Tool(String),
}

#[async_trait]
pub trait HarnessSubstrateBridge: Send + Sync {
    /// Return the combined wake-visible substrate inventory for `palette`.
    ///
    /// Must include both:
    /// - `FlavorRegistryFrozen::list_mcp_tools()` descriptors
    /// - `crate::personality::substrate_pack()` personality tools
    ///
    /// The returned specs are already palette-filtered by canonical id.
    fn list_harness_tools(&self, palette: &[String]) -> Vec<HarnessSubstrateToolSpec>;

    /// Dispatch one wake-scoped substrate call by canonical tool id.
    ///
    /// Implementations must preserve the live MCP semantics:
    /// registry tool first, then wake-scoped personality-tool fallback.
    async fn call_harness_tool(
        &self,
        call: HarnessSubstrateCall,
    ) -> Result<serde_json::Value, HarnessSubstrateError>;
}
```

If `Owner` is not already in scope, import it from `crate::Owner`.

- [ ] **Step 2: Implement the trait for `DevMcpServer`**

Open `crates/mcp-server/src/server.rs`. Implement the bridge beside the existing `DevMcpServer` methods:

```rust
#[async_trait::async_trait]
impl proxima_core::mcp::HarnessSubstrateBridge for DevMcpServer {
    fn list_harness_tools(
        &self,
        palette: &[String],
    ) -> Vec<proxima_core::mcp::HarnessSubstrateToolSpec> {
        let allows = |name: &str| palette.iter().any(|p| p == name);
        let mut out = Vec::new();

        for desc in self.registry().list_mcp_tools() {
            if allows(desc.name) {
                out.push(proxima_core::mcp::HarnessSubstrateToolSpec {
                    canonical_name: desc.name.to_string(),
                    description: desc.description.to_string(),
                    args_schema: desc.args_schema.clone(),
                });
            }
        }

        for tool in self.substrate_tools() {
            if allows(tool.tool_id()) {
                out.push(proxima_core::mcp::HarnessSubstrateToolSpec {
                    canonical_name: tool.tool_id().to_string(),
                    description: tool.description().to_string(),
                    args_schema: tool.args_schema(),
                });
            }
        }

        out
    }

    async fn call_harness_tool(
        &self,
        call: proxima_core::mcp::HarnessSubstrateCall,
    ) -> Result<serde_json::Value, proxima_core::mcp::HarnessSubstrateError> {
        let Some(engine) = self.engine.as_ref() else {
            return Err(proxima_core::mcp::HarnessSubstrateError::Tool(
                "wake-scoped substrate dispatch requires an attached engine".into(),
            ));
        };
        let Some(wake) = engine.wake_token_store().resolve(call.wake_token).await else {
            return Err(proxima_core::mcp::HarnessSubstrateError::MissingWakeContext);
        };
        if wake.owner != call.owner {
            return Err(proxima_core::mcp::HarnessSubstrateError::Unauthorized(
                call.canonical_name,
            ));
        }
        if !wake.palette.iter().any(|tool| tool == &call.canonical_name) {
            return Err(proxima_core::mcp::HarnessSubstrateError::Unauthorized(
                call.canonical_name,
            ));
        }

        let mut author = call.author;
        if author.caller_self_perspective.is_none() {
            author.caller_self_perspective = Some(wake.current_root_perspective_memory_id);
        }

        let auth = crate::auth::McpAuthContext {
            owner: wake.owner.clone(),
            scope: crate::auth::McpToolScope::Palette(wake.palette.clone()),
            model_id: Some(wake.model_id.clone()),
            wake: Some(wake),
            master_token_id: None,
        };

        self.call_tool(&call.canonical_name, call.args, author, Some(auth))
            .await
            .map_err(|err| match err {
                crate::server::ToolInvocationError::ToolNotFound(name) => {
                    proxima_core::mcp::HarnessSubstrateError::ToolNotFound(name)
                }
                crate::server::ToolInvocationError::Tool(tool_err) => {
                    map_tool_error(tool_err)
                }
            })
    }
}

fn map_tool_error(err: proxima_core::mcp::McpToolError) -> proxima_core::mcp::HarnessSubstrateError {
    match err {
        proxima_core::mcp::McpToolError::Storage(e) => {
            proxima_core::mcp::HarnessSubstrateError::Storage(e.to_string())
        }
        proxima_core::mcp::McpToolError::LayeringViolation(s) => {
            proxima_core::mcp::HarnessSubstrateError::Layering(s)
        }
        other => proxima_core::mcp::HarnessSubstrateError::Tool(other.to_string()),
    }
}
```

Load-bearing details:
- `call_harness_tool` deliberately calls `DevMcpServer::call_tool`, not `(descriptor.call)` directly. That preserves the existing registry-first / `call_personality_tool` fallback path.
- The bridge reconstructs `McpAuthContext` from the live `WakeTokenContext`; do not invent a parallel auth struct in the harness.
- Before constructing `McpToolCtx`, the bridge must default `McpAuthorContext.caller_self_perspective` from `WakeTokenContext.current_root_perspective_memory_id` when the caller did not supply one. This matches `handler::author_from_args` on the HTTP MCP path and keeps authoring tools such as `proxima-code/code_emit_execution_request` from failing with `caller_self_perspective is required...`.
- `list_harness_tools` must include `core/fetch_memory` and `core/emit_perspective` when those ids are in the palette.

Run: `cargo build -p proxima-core -p proxima-mcp-server`
Expected: clean.

- [ ] **Step 3: Take an `Arc<dyn HarnessSubstrateBridge>` in `HarnessLoop`**

`HarnessLoop` (Task 14) owns `Arc<Engine>` plus the bridge:

```rust
pub struct HarnessLoop {
    engine: Arc<proxima_core::Engine>,
    substrate_bridge: Arc<dyn proxima_core::mcp::HarnessSubstrateBridge>,
    // ...other fields from Task 14...
}

impl HarnessLoop {
    pub fn new(
        engine: Arc<proxima_core::Engine>,
        substrate_bridge: Arc<dyn proxima_core::mcp::HarnessSubstrateBridge>,
    ) -> Self {
        Self { engine, substrate_bridge, /* ... */ }
    }
}
```

Every binary that constructs `HarnessLoop` passes the existing `Arc<DevMcpServer>` as `Arc<dyn HarnessSubstrateBridge>`.

- [ ] **Step 4: Implement `crates/harness/src/tools/substrate_dispatch.rs`**

```rust
//! In-process dispatch into wake-visible substrate tools.
//!
//! Calls the injected `HarnessSubstrateBridge`, which is implemented by
//! `DevMcpServer` and preserves the live MCP behavior: registry MCP
//! tools plus wake-scoped personality substrate-pack tools.

use std::sync::Arc;

use proxima_core::harness::{HarnessContext, SubstrateToolBinding};
use proxima_core::mcp::{
    HarnessSubstrateBridge, HarnessSubstrateCall, HarnessSubstrateError, McpAuthorContext,
};
use serde_json::Value;

#[derive(Debug, Clone)]
pub enum SubstrateDispatchResult {
    Ok(Value),
    Recoverable(String),
    Fatal(String),
}

pub async fn dispatch(
    bridge: &Arc<dyn HarnessSubstrateBridge>,
    binding: &SubstrateToolBinding,
    args: Value,
    ctx: &HarnessContext,
    model_id: &str,
) -> SubstrateDispatchResult {
    let author = McpAuthorContext {
        model_id: model_id.to_string(),
        client_name: "proxima-harness".to_string(),
        client_version: env!("CARGO_PKG_VERSION").to_string(),
        caller_self_perspective: Some(ctx.root_perspective_memory_id),
    };

    let call = HarnessSubstrateCall {
        canonical_name: binding.canonical_name.clone(),
        args,
        owner: ctx.owner.clone(),
        wake_token: ctx.wake_token,
        author,
    };

    match bridge.call_harness_tool(call).await {
        Ok(v) => SubstrateDispatchResult::Ok(v),
        Err(HarnessSubstrateError::Storage(e) | HarnessSubstrateError::Layering(e)) => {
            SubstrateDispatchResult::Fatal(e)
        }
        Err(other) => SubstrateDispatchResult::Recoverable(other.to_string()),
    }
}
```

- [ ] **Step 5: Add dispatch inventory regression**

Create or extend `crates/harness/tests/substrate_dispatch.rs` with a test using a real `DevMcpServer` bridge:

1. Build a palette containing `core/fetch_memory`, `core/emit_perspective`, and one registry MCP tool such as `core/list_substrate_tools`.
2. Call `bridge.list_harness_tools(&palette)`.
3. Assert all three canonical ids are present.
4. Assert provider-safe names are generated by the harness program builder from those canonical ids (`core_fetch_memory`, `core_emit_perspective`, `core_list_substrate_tools`).

Also add a wake-auth regression for caller self:

1. Mint a `WakeTokenContext` with `current_root_perspective_memory_id = root`.
2. Call a registry MCP authoring tool through `HarnessSubstrateBridge::call_harness_tool` with `McpAuthorContext.caller_self_perspective = None`.
3. Assert the invoked tool sees `McpToolCtx.caller_self_perspective == Some(root)`.
4. Include the real failure string in the regression name or assertion context: `caller_self_perspective is required`.

This is the regression for the previous plan bug: registry-only discovery must fail this test because it cannot list `core/fetch_memory` / `core/emit_perspective`.

- [ ] **Step 6: Build**

Run: `cargo build -p proxima-harness -p proxima-core -p proxima-mcp-server`
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add crates/core/src/mcp/mod.rs crates/mcp-server/src/server.rs \
        crates/harness/src/tools/substrate_dispatch.rs \
        crates/harness/tests/substrate_dispatch.rs
git commit -m "$(cat <<'EOF'
harness: preserve substrate-pack tool dispatch via DevMcpServer bridge

Adds a core-owned HarnessSubstrateBridge implemented by DevMcpServer.
The harness lists and calls the same wake-visible tool surface as the
HTTP MCP path: registry MCP tools plus personality substrate-pack tools.
This preserves core/fetch_memory and core/emit_perspective for existing
Code Engineer wake palettes.
EOF
)"
```
