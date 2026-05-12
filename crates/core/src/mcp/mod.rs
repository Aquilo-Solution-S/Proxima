//! Build-time MCP tool registration for local development adapters.
//!
//! Composite binaries register tools through flavor crates at startup;
//! there is no runtime registration path.

pub mod core_tools;
pub mod handles;

pub use core_tools::{
    AuditEmit, PersonalityConfigChangedCaller, PersonalityConfigChangedSubject,
    PersonalityConfigChangedV1, PersonalityConfigChangedVerb, emit_personality_config_changed,
};
pub use handles::{EntityRef, Handle, HandleTable};

use std::sync::Arc;

use futures::future::BoxFuture;

use crate::{MemoryId, Owner, verbs::schema::FlavorRegistryFrozen};

#[derive(Debug, Clone)]
pub struct McpAuthorContext {
    pub model_id: String,
    pub client_name: String,
    pub client_version: String,
    pub caller_self_perspective: Option<MemoryId>,
}

#[derive(Clone)]
pub struct McpToolCtx {
    pub pool: sqlx::PgPool,
    pub owner: Owner,
    pub handles: Arc<HandleTable>,
    pub registry: Arc<FlavorRegistryFrozen>,
    pub author: McpAuthorContext,
    pub caller_self_perspective: Option<MemoryId>,
    /// Set by `McpToolHost::call_tool` for master-token requests so
    /// downstream code (notably the audit emit path) can distinguish
    /// master-token from wake-token callers without inspecting the
    /// auth context. `None` for wake-token, no-auth, or test calls.
    pub master_token_id: Option<uuid::Uuid>,
    /// `Some` when the MCP server was constructed with `with_engine`.
    /// Tools that need to call engine verbs (CRUD-via-MCP) require this;
    /// pure read-only / projection tools can ignore it.
    pub engine: Option<Arc<crate::Engine>>,
}

impl std::fmt::Debug for McpToolCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpToolCtx")
            .field("owner", &self.owner)
            .field("author", &self.author)
            .finish_non_exhaustive()
    }
}

impl McpToolCtx {
    /// `None` when the MCP server is running without a wired engine
    /// (early test scaffolds). Real deployments always wire an engine.
    #[must_use]
    pub fn engine(&self) -> Option<&crate::Engine> {
        self.engine.as_deref()
    }

    /// Convenience: storage handle bound to the engine. Same scope as
    /// `engine()` — only available when an engine is attached.
    #[must_use]
    pub fn storage(&self) -> Option<&dyn crate::Storage> {
        self.engine.as_ref().map(|e| &**e.storage())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum McpToolError {
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("unknown handle: {0}")]
    UnknownHandle(String),
    #[error("layering violation: {0}")]
    LayeringViolation(String),
    #[error("storage: {0}")]
    Storage(#[from] crate::StorageError),
    #[error("{0}")]
    Other(String),
}

#[derive(Debug, Clone)]
pub struct McpToolDescriptor {
    pub name: &'static str,
    pub description: &'static str,
    pub produces_schema_ids: &'static [&'static str],
    pub args_schema: serde_json::Value,
    pub call: McpCallFn,
}

pub type McpCallFn = fn(
    McpToolCtx,
    serde_json::Value,
) -> BoxFuture<'static, Result<serde_json::Value, McpToolError>>;

pub trait McpTool: Send + Sync + 'static {
    const NAME: &'static str;
    const DESCRIPTION: &'static str;
    const PRODUCES_SCHEMA_IDS: &'static [&'static str] = &[];

    type Args: serde::de::DeserializeOwned + schemars::JsonSchema + Send + 'static;
    type Output: serde::Serialize + Send + 'static;

    fn call(
        ctx: McpToolCtx,
        args: Self::Args,
    ) -> BoxFuture<'static, Result<Self::Output, McpToolError>>;
}

/// Tool names exposed to LLM-hosted MCP clients must also be valid
/// provider function names. Internal ids use flavor-style `/`
/// separators, which some runners pass through unchanged.
#[must_use]
pub fn provider_safe_tool_name(canonical: &str) -> String {
    let mut out = String::with_capacity(canonical.len());
    let mut previous_dot = false;
    for ch in canonical.chars() {
        let safe = ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' || ch == '.';
        let mapped = if safe { ch } else { '_' };
        if mapped == '.' {
            if previous_dot {
                out.push('_');
                previous_dot = false;
            } else {
                out.push(mapped);
                previous_dot = true;
            }
        } else {
            out.push(mapped);
            previous_dot = false;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::provider_safe_tool_name;

    #[test]
    fn provider_safe_tool_name_replaces_runner_invalid_separators() {
        assert_eq!(
            provider_safe_tool_name("core/emit_abstraction"),
            "core_emit_abstraction"
        );
        assert_eq!(
            provider_safe_tool_name("proxima-mcp/proxima_remember"),
            "proxima-mcp_proxima_remember"
        );
        assert_eq!(provider_safe_tool_name("a..b"), "a._b");
    }
}

#[cfg(test)]
mod ctx_engine_tests {
    use super::*;
    use crate::auth::NoAuth;
    use crate::verbs::query::MemoryStore;
    use crate::{Engine, FlavorRegistry, OrgId, Owner, Principal, UserId};
    use std::sync::Arc;

    #[tokio::test]
    async fn ctx_storage_returns_none_when_engine_unwired() {
        let owner = Owner {
            principal: Principal::User(UserId::new(uuid::Uuid::now_v7())),
            org_id: OrgId::new(uuid::Uuid::now_v7()),
        };
        let pool = sqlx::PgPool::connect_lazy("postgres://x/x").expect("lazy");
        let ctx = McpToolCtx {
            pool,
            owner: owner.clone(),
            handles: Arc::new(HandleTable::new()),
            registry: Arc::new(FlavorRegistry::new().freeze()),
            author: McpAuthorContext {
                model_id: "t".into(),
                client_name: "t".into(),
                client_version: "0".into(),
                caller_self_perspective: None,
            },
            caller_self_perspective: None,
            master_token_id: None,
            engine: None,
        };
        assert!(ctx.storage().is_none());
        assert!(ctx.engine().is_none());
    }

    #[tokio::test]
    async fn ctx_storage_returns_some_when_engine_wired() {
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
        let pool = sqlx::PgPool::connect_lazy("postgres://x/x").expect("lazy");
        let ctx = McpToolCtx {
            pool,
            owner,
            handles: Arc::new(HandleTable::new()),
            registry: Arc::new(FlavorRegistry::new().freeze()),
            author: McpAuthorContext {
                model_id: "t".into(),
                client_name: "t".into(),
                client_version: "0".into(),
                caller_self_perspective: None,
            },
            caller_self_perspective: None,
            master_token_id: None,
            engine: Some(engine.clone()),
        };
        assert!(ctx.engine().is_some());
        assert!(ctx.storage().is_some());
    }
}
