//! Build-time MCP tool registration for local development adapters.
//!
//! Composite binaries register tools through flavor crates at startup;
//! there is no runtime registration path.

pub mod core_tools;
pub mod handles;
pub(crate) mod schema;

pub use core_tools::{
    AuditEmit, PersonalityConfigChangedCaller, PersonalityConfigChangedSubject,
    PersonalityConfigChangedV1, PersonalityConfigChangedVerb,
};
pub use handles::{
    EntityKind, EntityRef, Handle, HandleTable, MemoryHandleClass, PrefixedUuidClass,
    PrefixedUuidError, ResolveError, format_prefixed_uuid, parse_prefixed_uuid,
};

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::Arc;

use futures::future::BoxFuture;

use crate::authz::AuthzContext;
use crate::{
    EdgeId, GoalId, MemoryId, Owner, PersonalityInstanceId, verbs::schema::FlavorRegistryFrozen,
};

#[derive(Debug, Clone)]
pub struct McpAuthorContext {
    pub model_id: String,
    pub client_name: String,
    pub client_version: String,
    pub personality_instance_id: Option<PersonalityInstanceId>,
    pub caller_self_perspective: Option<MemoryId>,
}

/// Selects the regime that `McpToolCtx::format_*` / `resolve_*`
/// helpers operate in.
///
/// - `Handles`: handle-projected, model-facing. Emits/parses handle
///   strings (`F1`, `A1`, `P1`, `G7`, …) against a `HandleTable`.
/// - `RawIds`: master-token / human-facing. Emits/parses raw UUID
///   strings. No `HandleTable` is consulted.
/// - `PrefixedIds`: wire-facing. Emits/parses typed `F:<uuid>`,
///   `A:<uuid>`, `P:<uuid>`, `G:<uuid>`, `I:<uuid>`, `E:<uuid>`,
///   and `W:<uuid>` strings. No `HandleTable` is consulted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    Handles,
    RawIds,
    PrefixedIds,
}

#[derive(Clone, Default)]
pub struct McpToolExtensions {
    values: Arc<HashMap<TypeId, Arc<dyn Any + Send + Sync>>>,
}

impl std::fmt::Debug for McpToolExtensions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpToolExtensions")
            .field("len", &self.values.len())
            .finish_non_exhaustive()
    }
}

impl McpToolExtensions {
    #[must_use]
    pub fn with<T>(value: T) -> Self
    where
        T: Send + Sync + 'static,
    {
        let mut extensions = Self::default();
        extensions.insert(value);
        extensions
    }

    pub fn insert<T>(&mut self, value: T) -> Option<Arc<T>>
    where
        T: Send + Sync + 'static,
    {
        Arc::make_mut(&mut self.values)
            .insert(TypeId::of::<T>(), Arc::new(value))
            .and_then(|old| old.downcast::<T>().ok())
    }

    #[must_use]
    pub fn get<T>(&self) -> Option<Arc<T>>
    where
        T: Send + Sync + 'static,
    {
        self.values
            .get(&TypeId::of::<T>())
            .cloned()
            .and_then(|value| value.downcast::<T>().ok())
    }
}

#[derive(Clone)]
pub struct McpToolCtx {
    pub owner: Owner,
    /// Caller's authorization context, threaded from the transport
    /// edge. Tools pass this to engine verbs — never a substituted
    /// engine identity (privilege-escalation guard).
    pub authz: AuthzContext,
    /// `Some` for wake-dispatched calls (table provided by the wake);
    /// `None` for master-token / unauthenticated calls. Must be `Some`
    /// when `mode == OutputMode::Handles`.
    pub handles: Option<Arc<HandleTable>>,
    pub mode: OutputMode,
    pub registry: Arc<FlavorRegistryFrozen>,
    pub author: McpAuthorContext,
    pub caller_self_perspective: Option<MemoryId>,
    /// Set by `McpToolHost::call_tool` for master-token requests so
    /// downstream code (notably the audit emit path) can distinguish
    /// master-token from wake-token callers without inspecting the
    /// auth context. `None` for wake-token, no-auth, or test calls.
    pub master_token_id: Option<uuid::Uuid>,
    /// Backend/flavor services supplied by the host. Core does not name
    /// concrete service types; PG-aware flavors may downcast their own
    /// dependencies here.
    pub extensions: McpToolExtensions,
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

    #[must_use]
    pub fn extension<T>(&self) -> Option<Arc<T>>
    where
        T: Send + Sync + 'static,
    {
        self.extensions.get::<T>()
    }

    fn handle_table(&self) -> &HandleTable {
        self.handles
            .as_ref()
            .expect("OutputMode::Handles requires a HandleTable")
    }

    #[must_use]
    pub fn format_memory_with_class(&self, id: MemoryId, class: MemoryHandleClass) -> String {
        match class {
            MemoryHandleClass::Fact => self.format_fact_memory(id),
            MemoryHandleClass::Abstraction => self.format_abstraction_memory(id),
            MemoryHandleClass::Perspective => self.format_perspective_memory(id),
        }
    }

    #[must_use]
    pub fn format_fact_memory(&self, id: MemoryId) -> String {
        match self.mode {
            OutputMode::Handles => self
                .handle_table()
                .assign_fact_memory(id)
                .as_str()
                .to_string(),
            OutputMode::RawIds => id.into_inner().to_string(),
            OutputMode::PrefixedIds => {
                format_prefixed_uuid(id.into_inner(), PrefixedUuidClass::Fact)
            }
        }
    }

    #[must_use]
    pub fn format_abstraction_memory(&self, id: MemoryId) -> String {
        match self.mode {
            OutputMode::Handles => self
                .handle_table()
                .assign_abstraction_memory(id)
                .as_str()
                .to_string(),
            OutputMode::RawIds => id.into_inner().to_string(),
            OutputMode::PrefixedIds => {
                format_prefixed_uuid(id.into_inner(), PrefixedUuidClass::Abstraction)
            }
        }
    }

    #[must_use]
    pub fn format_perspective_memory(&self, id: MemoryId) -> String {
        match self.mode {
            OutputMode::Handles => self
                .handle_table()
                .assign_perspective_memory(id)
                .as_str()
                .to_string(),
            OutputMode::RawIds => id.into_inner().to_string(),
            OutputMode::PrefixedIds => {
                format_prefixed_uuid(id.into_inner(), PrefixedUuidClass::Perspective)
            }
        }
    }

    #[must_use]
    pub fn format_goal(&self, id: GoalId) -> String {
        match self.mode {
            OutputMode::Handles => self.handle_table().assign_goal(id).as_str().to_string(),
            OutputMode::RawIds => id.into_inner().to_string(),
            OutputMode::PrefixedIds => {
                format_prefixed_uuid(id.into_inner(), PrefixedUuidClass::Goal)
            }
        }
    }

    #[must_use]
    pub fn format_edge(&self, id: EdgeId) -> String {
        match self.mode {
            OutputMode::Handles => self.handle_table().assign_edge(id).as_str().to_string(),
            OutputMode::RawIds => id.into_inner().to_string(),
            OutputMode::PrefixedIds => {
                format_prefixed_uuid(id.into_inner(), PrefixedUuidClass::Edge)
            }
        }
    }

    #[must_use]
    pub fn format_personality(&self, id: PersonalityInstanceId) -> String {
        match self.mode {
            OutputMode::Handles => self
                .handle_table()
                .assign_personality(id)
                .as_str()
                .to_string(),
            OutputMode::RawIds => id.into_inner().to_string(),
            OutputMode::PrefixedIds => {
                format_prefixed_uuid(id.into_inner(), PrefixedUuidClass::Personality)
            }
        }
    }

    #[must_use]
    pub fn format_wake_entry(&self, id: uuid::Uuid) -> String {
        match self.mode {
            OutputMode::Handles => self
                .handle_table()
                .assign_wake_entry(id)
                .as_str()
                .to_string(),
            OutputMode::RawIds => id.to_string(),
            OutputMode::PrefixedIds => format_prefixed_uuid(id, PrefixedUuidClass::WakeEntry),
        }
    }

    #[must_use]
    pub fn format_flavor_object(&self, kind: &str, id: uuid::Uuid, prefix: char) -> String {
        match self.mode {
            OutputMode::Handles => self
                .handle_table()
                .assign_flavor_object(kind, id, prefix)
                .as_str()
                .to_string(),
            OutputMode::RawIds => id.to_string(),
            OutputMode::PrefixedIds => format!("{prefix}:{id}"),
        }
    }

    /// Parse `raw` as a memory reference under the active mode.
    ///
    /// # Errors
    ///
    /// Returns `McpToolError::Resolve` in `Handles` mode if the handle
    /// is unknown or names the wrong kind, and `McpToolError::InvalidInput`
    /// in `RawIds` mode if `raw` is not a well-formed UUID.
    pub fn resolve_memory(&self, raw: &str) -> Result<MemoryId, McpToolError> {
        match self.mode {
            OutputMode::Handles => self
                .handle_table()
                .resolve_memory(raw)
                .map_err(McpToolError::Resolve),
            OutputMode::RawIds => raw
                .parse::<uuid::Uuid>()
                .map(MemoryId::new)
                .map_err(|e| McpToolError::InvalidInput(format!("not a uuid: {e}"))),
            OutputMode::PrefixedIds => parse_any_prefixed_memory_uuid(raw).map(MemoryId::new),
        }
    }

    /// Parse `raw` as a fact-memory reference under the active mode.
    ///
    /// # Errors
    ///
    /// See [`McpToolCtx::resolve_memory`].
    pub fn resolve_fact_memory(&self, raw: &str) -> Result<MemoryId, McpToolError> {
        match self.mode {
            OutputMode::Handles => self
                .handle_table()
                .resolve_fact_memory(raw)
                .map_err(McpToolError::Resolve),
            OutputMode::RawIds => raw
                .parse::<uuid::Uuid>()
                .map(MemoryId::new)
                .map_err(|e| McpToolError::InvalidInput(format!("not a uuid: {e}"))),
            OutputMode::PrefixedIds => parse_prefixed_uuid(raw, PrefixedUuidClass::Fact)
                .map(MemoryId::new)
                .map_err(|e| McpToolError::InvalidInput(e.to_string())),
        }
    }

    /// Parse `raw` as an abstraction-memory reference under the active mode.
    ///
    /// # Errors
    ///
    /// See [`McpToolCtx::resolve_memory`].
    pub fn resolve_abstraction_memory(&self, raw: &str) -> Result<MemoryId, McpToolError> {
        match self.mode {
            OutputMode::Handles => self
                .handle_table()
                .resolve_abstraction_memory(raw)
                .map_err(McpToolError::Resolve),
            OutputMode::RawIds => raw
                .parse::<uuid::Uuid>()
                .map(MemoryId::new)
                .map_err(|e| McpToolError::InvalidInput(format!("not a uuid: {e}"))),
            OutputMode::PrefixedIds => parse_prefixed_uuid(raw, PrefixedUuidClass::Abstraction)
                .map(MemoryId::new)
                .map_err(|e| McpToolError::InvalidInput(e.to_string())),
        }
    }

    /// Parse `raw` as a perspective-memory reference under the active mode.
    ///
    /// # Errors
    ///
    /// See [`McpToolCtx::resolve_memory`].
    pub fn resolve_perspective_memory(&self, raw: &str) -> Result<MemoryId, McpToolError> {
        match self.mode {
            OutputMode::Handles => self
                .handle_table()
                .resolve_perspective_memory(raw)
                .map_err(McpToolError::Resolve),
            OutputMode::RawIds => raw
                .parse::<uuid::Uuid>()
                .map(MemoryId::new)
                .map_err(|e| McpToolError::InvalidInput(format!("not a uuid: {e}"))),
            OutputMode::PrefixedIds => parse_prefixed_uuid(raw, PrefixedUuidClass::Perspective)
                .map(MemoryId::new)
                .map_err(|e| McpToolError::InvalidInput(e.to_string())),
        }
    }

    /// Parse `raw` as a goal reference under the active mode.
    ///
    /// # Errors
    ///
    /// See [`McpToolCtx::resolve_memory`].
    pub fn resolve_goal(&self, raw: &str) -> Result<GoalId, McpToolError> {
        match self.mode {
            OutputMode::Handles => self
                .handle_table()
                .resolve_goal(raw)
                .map_err(McpToolError::Resolve),
            OutputMode::RawIds => raw
                .parse::<uuid::Uuid>()
                .map(GoalId::new)
                .map_err(|e| McpToolError::InvalidInput(format!("not a uuid: {e}"))),
            OutputMode::PrefixedIds => parse_prefixed_uuid(raw, PrefixedUuidClass::Goal)
                .map(GoalId::new)
                .map_err(|e| McpToolError::InvalidInput(e.to_string())),
        }
    }

    /// Parse `raw` as an edge reference under the active mode.
    ///
    /// # Errors
    ///
    /// See [`McpToolCtx::resolve_memory`].
    pub fn resolve_edge(&self, raw: &str) -> Result<EdgeId, McpToolError> {
        match self.mode {
            OutputMode::Handles => self
                .handle_table()
                .resolve_edge(raw)
                .map_err(McpToolError::Resolve),
            OutputMode::RawIds => raw
                .parse::<uuid::Uuid>()
                .map(EdgeId::new)
                .map_err(|e| McpToolError::InvalidInput(format!("not a uuid: {e}"))),
            OutputMode::PrefixedIds => parse_prefixed_uuid(raw, PrefixedUuidClass::Edge)
                .map(EdgeId::new)
                .map_err(|e| McpToolError::InvalidInput(e.to_string())),
        }
    }

    /// Parse `raw` as a personality reference under the active mode.
    ///
    /// # Errors
    ///
    /// See [`McpToolCtx::resolve_memory`].
    pub fn resolve_personality(&self, raw: &str) -> Result<PersonalityInstanceId, McpToolError> {
        match self.mode {
            OutputMode::Handles => self
                .handle_table()
                .resolve_personality(raw)
                .map_err(McpToolError::Resolve),
            OutputMode::RawIds => raw
                .parse::<uuid::Uuid>()
                .map(PersonalityInstanceId::new)
                .map_err(|e| McpToolError::InvalidInput(format!("not a uuid: {e}"))),
            OutputMode::PrefixedIds => parse_prefixed_uuid(raw, PrefixedUuidClass::Personality)
                .map(PersonalityInstanceId::new)
                .map_err(|e| McpToolError::InvalidInput(e.to_string())),
        }
    }

    /// Parse `raw` as a wake-entry reference under the active mode.
    ///
    /// # Errors
    ///
    /// See [`McpToolCtx::resolve_memory`].
    pub fn resolve_wake_entry(&self, raw: &str) -> Result<uuid::Uuid, McpToolError> {
        match self.mode {
            OutputMode::Handles => self
                .handle_table()
                .resolve_wake_entry(raw)
                .map_err(McpToolError::Resolve),
            OutputMode::RawIds => raw
                .parse::<uuid::Uuid>()
                .map_err(|e| McpToolError::InvalidInput(format!("not a uuid: {e}"))),
            OutputMode::PrefixedIds => parse_prefixed_uuid(raw, PrefixedUuidClass::WakeEntry)
                .map_err(|e| McpToolError::InvalidInput(e.to_string())),
        }
    }

    /// Parse `raw` as a flavor-object reference of the given `kind`
    /// under the active mode.
    ///
    /// # Errors
    ///
    /// See [`McpToolCtx::resolve_memory`].
    pub fn resolve_flavor_object(&self, raw: &str, kind: &str) -> Result<uuid::Uuid, McpToolError> {
        match self.mode {
            OutputMode::Handles => self
                .handle_table()
                .resolve_flavor_object(raw, kind)
                .map_err(McpToolError::Resolve),
            OutputMode::RawIds => raw
                .parse::<uuid::Uuid>()
                .map_err(|e| McpToolError::InvalidInput(format!("not a uuid: {e}"))),
            OutputMode::PrefixedIds => parse_flavor_prefixed_uuid(raw),
        }
    }
}

fn parse_any_prefixed_memory_uuid(raw: &str) -> Result<uuid::Uuid, McpToolError> {
    let class = match raw.split_once(':').map(|(prefix, _)| prefix) {
        Some("F") => PrefixedUuidClass::Fact,
        Some("A") => PrefixedUuidClass::Abstraction,
        Some("P") => PrefixedUuidClass::Perspective,
        Some(prefix) => {
            return Err(McpToolError::InvalidInput(format!(
                "expected memory id prefix F, A, or P; got '{prefix}' in '{raw}'"
            )));
        }
        None => {
            return Err(McpToolError::InvalidInput(format!(
                "malformed memory id '{raw}': expected F:<uuid>, A:<uuid>, or P:<uuid>"
            )));
        }
    };
    parse_prefixed_uuid(raw, class).map_err(|e| McpToolError::InvalidInput(e.to_string()))
}

fn parse_flavor_prefixed_uuid(raw: &str) -> Result<uuid::Uuid, McpToolError> {
    let Some((prefix, uuid_part)) = raw.split_once(':') else {
        return Err(McpToolError::InvalidInput(format!(
            "malformed flavor object id '{raw}': expected <prefix>:<uuid>"
        )));
    };
    let mut chars = prefix.chars();
    if !matches!(chars.next(), Some(c) if c.is_ascii_uppercase()) || chars.next().is_some() {
        return Err(McpToolError::InvalidInput(format!(
            "malformed flavor object id '{raw}': prefix must be one ASCII uppercase letter"
        )));
    }
    uuid_part
        .parse::<uuid::Uuid>()
        .map_err(|e| McpToolError::InvalidInput(format!("not a uuid: {e}")))
}

#[derive(Debug, thiserror::Error)]
pub enum McpToolError {
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("{0}")]
    Resolve(ResolveError),
    #[error("{0}")]
    Protocol(#[from] crate::error::ProtocolError),
    #[error("layering violation: {0}")]
    LayeringViolation(String),
    #[error("storage: {0}")]
    Storage(#[from] crate::StorageError),
    #[error("{0}")]
    Other(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpToolErrorKind {
    InvalidInput,
    InvalidRequest,
    Internal,
}

impl McpToolError {
    #[must_use]
    pub fn kind(&self) -> McpToolErrorKind {
        match self {
            Self::InvalidInput(_) | Self::Resolve(_) => McpToolErrorKind::InvalidInput,
            Self::Protocol(e) => match e.code {
                crate::error::ErrorCode::InvalidArgument => McpToolErrorKind::InvalidInput,
                crate::error::ErrorCode::Internal => McpToolErrorKind::Internal,
                crate::error::ErrorCode::AuthRequired
                | crate::error::ErrorCode::Forbidden
                | crate::error::ErrorCode::UnknownSchema
                | crate::error::ErrorCode::AlreadyIngested
                | crate::error::ErrorCode::IdempotencyConflict
                | crate::error::ErrorCode::NotFound
                | crate::error::ErrorCode::ToolNotRegistered
                | crate::error::ErrorCode::TriggerConflict
                | crate::error::ErrorCode::DuplicateTriggerInRequest => {
                    McpToolErrorKind::InvalidRequest
                }
            },
            Self::LayeringViolation(_) => McpToolErrorKind::InvalidRequest,
            Self::Storage(storage) => match storage {
                crate::StorageError::ConstraintViolation(_) | crate::StorageError::NotFound => {
                    McpToolErrorKind::InvalidInput
                }
                crate::StorageError::Conflict(_) => McpToolErrorKind::InvalidRequest,
                crate::StorageError::Unavailable(_) | crate::StorageError::Internal(_) => {
                    McpToolErrorKind::Internal
                }
            },
            Self::Other(_) => McpToolErrorKind::Internal,
        }
    }

    #[must_use]
    pub fn client_message(&self) -> String {
        match self.kind() {
            McpToolErrorKind::InvalidInput | McpToolErrorKind::InvalidRequest => self.to_string(),
            McpToolErrorKind::Internal => "internal server error".to_string(),
        }
    }
}

impl From<ResolveError> for McpToolError {
    fn from(e: ResolveError) -> Self {
        McpToolError::Resolve(e)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpToolOrigin {
    Substrate,
    Flavor(String),
}

#[derive(Debug, Clone)]
pub struct McpToolDescriptor {
    pub name: &'static str,
    pub description: &'static str,
    pub origin: McpToolOrigin,
    pub produces_schema_ids: &'static [&'static str],
    pub args_schema: serde_json::Value,
    pub action_arg_specs: &'static [McpActionArgSpec],
    pub call: McpCallFn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct McpActionArgSpec {
    pub action: &'static str,
    pub allowed_fields: &'static [&'static str],
    pub required_fields: &'static [&'static str],
}

pub(crate) fn validate_action_args(
    tool_name: &str,
    specs: &[McpActionArgSpec],
    args: &serde_json::Value,
) -> Result<(), McpToolError> {
    if specs.is_empty() {
        return Ok(());
    }
    let object = args.as_object().ok_or_else(|| {
        McpToolError::InvalidInput(format!("{tool_name} arguments must be a JSON object"))
    })?;
    let action = object
        .get("action")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            McpToolError::InvalidInput(format!(
                "{tool_name} arguments must include string field `action`"
            ))
        })?;
    let spec = specs
        .iter()
        .find(|spec| spec.action == action)
        .ok_or_else(|| {
            let supported = specs
                .iter()
                .map(|spec| spec.action)
                .collect::<Vec<_>>()
                .join(", ");
            McpToolError::InvalidInput(format!(
                "{tool_name} action `{action}` is not supported; expected one of: {supported}"
            ))
        })?;

    let allowed = spec
        .allowed_fields
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    let mut unexpected = object
        .keys()
        .filter(|field| field.as_str() != "action" && !allowed.contains(field.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    if !unexpected.is_empty() {
        unexpected.sort();
        return Err(McpToolError::InvalidInput(format!(
            "{tool_name} action `{action}` does not accept field(s): {}",
            unexpected.join(", ")
        )));
    }

    let missing = spec
        .required_fields
        .iter()
        .copied()
        .filter(|field| !object.contains_key(*field))
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(McpToolError::InvalidInput(format!(
            "{tool_name} action `{action}` requires field(s): {}",
            missing.join(", ")
        )));
    }
    Ok(())
}

pub type McpCallFn = fn(
    McpToolCtx,
    serde_json::Value,
) -> BoxFuture<'static, Result<serde_json::Value, McpToolError>>;

pub trait McpTool: Send + Sync + 'static {
    const NAME: &'static str;
    const DESCRIPTION: &'static str;
    const PRODUCES_SCHEMA_IDS: &'static [&'static str] = &[];
    const ACTION_ARG_SPECS: &'static [McpActionArgSpec] = &[];

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

#[must_use]
pub fn tool_name_matches(canonical: &str, request_name: &str) -> bool {
    canonical == request_name || provider_safe_tool_name(canonical) == request_name
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
pub struct McpToolAnnotations {
    pub read_only: Option<bool>,
    pub destructive: Option<bool>,
    pub idempotent: Option<bool>,
    pub open_world: Option<bool>,
}

impl McpToolAnnotations {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            read_only: None,
            destructive: None,
            idempotent: None,
            open_world: None,
        }
    }

    #[must_use]
    pub const fn read_only(mut self, value: bool) -> Self {
        self.read_only = Some(value);
        self
    }

    #[must_use]
    pub const fn destructive(mut self, value: bool) -> Self {
        self.destructive = Some(value);
        self
    }

    #[must_use]
    pub const fn idempotent(mut self, value: bool) -> Self {
        self.idempotent = Some(value);
        self
    }

    #[must_use]
    pub const fn open_world(mut self, value: bool) -> Self {
        self.open_world = Some(value);
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoreActionMeta {
    pub tool: &'static str,
    pub action: &'static str,
    pub scope_key: &'static str,
    pub description: &'static str,
    pub produces_schema_ids: &'static [&'static str],
    pub annotations: McpToolAnnotations,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoreResourceMeta {
    pub uri_template: &'static str,
    pub name: &'static str,
    pub title: &'static str,
    pub scope_key: &'static str,
    pub description: &'static str,
    pub is_template: bool,
}

pub const CORE_RESOURCES: &[CoreResourceMeta] = &[
    CoreResourceMeta {
        uri_template: "proxima://schemas{?kind}",
        name: "proxima-schemas",
        title: "Proxima Schemas",
        scope_key: "resource:schemas",
        description: "Registered core and flavor schema catalog, optionally filtered by payload kind.",
        is_template: false,
    },
    CoreResourceMeta {
        uri_template: "proxima://edge-types",
        name: "proxima-edge-types",
        title: "Proxima Edge Types",
        scope_key: "resource:edge-types",
        description: "Registered relation descriptors and relation classes.",
        is_template: false,
    },
    CoreResourceMeta {
        uri_template: "proxima://tools",
        name: "proxima-tools",
        title: "Proxima Tools",
        scope_key: "resource:tools",
        description: "Registered substrate and flavor MCP tool catalog visible to the caller.",
        is_template: false,
    },
    CoreResourceMeta {
        uri_template: "proxima://graph{?include_tombstoned}",
        name: "proxima-graph",
        title: "Proxima Graph",
        scope_key: "resource:graph",
        description: "Owner-scoped personality graph plus schema, edge-type, and tool catalogs.",
        is_template: false,
    },
    CoreResourceMeta {
        uri_template: "proxima://memory/{id}{?expand_neighbors}",
        name: "proxima-memory",
        title: "Proxima Memory",
        scope_key: "resource:memory",
        description: "Owner-scoped memory by prefixed id, raw id, or handle.",
        is_template: true,
    },
    CoreResourceMeta {
        uri_template: "proxima://memory/{id}/lineage{?direction,depth,limit}",
        name: "proxima-memory-lineage",
        title: "Proxima Memory Lineage",
        scope_key: "resource:memory-lineage",
        description: "Owner-scoped Provenance/Supersession lineage from a memory id or handle.",
        is_template: true,
    },
    CoreResourceMeta {
        uri_template: "proxima://events{?since,limit}",
        name: "proxima-events",
        title: "Proxima Events",
        scope_key: "resource:events",
        description: "Owner-scoped change-event pull log.",
        is_template: true,
    },
];

#[must_use = "iterators are lazy and must be consumed"]
pub fn all_core_resources() -> impl Iterator<Item = &'static CoreResourceMeta> {
    CORE_RESOURCES.iter()
}

#[must_use = "iterators are lazy and must be consumed"]
pub fn all_core_actions() -> impl Iterator<Item = &'static CoreActionMeta> {
    core_tools::goal::CORE_GOAL_ACTIONS
        .iter()
        .chain(core_tools::wake::CORE_WAKE_ACTIONS.iter())
        .chain(core_tools::personality::CORE_PERSONALITY_ACTIONS.iter())
        .chain(core_tools::fact::CORE_FACT_ACTIONS.iter())
}

#[must_use]
pub fn core_action_meta(tool: &str, action: &str) -> Option<&'static CoreActionMeta> {
    all_core_actions().find(|meta| meta.tool == tool && meta.action == action)
}

#[must_use]
pub fn core_tool_has_actions(tool: &str) -> bool {
    all_core_actions().any(|meta| meta.tool == tool)
}

/// MCP behavior hints for substrate tools, keyed by registered tool name.
#[must_use]
pub fn core_tool_annotations(canonical_name: &str) -> Option<McpToolAnnotations> {
    let base = McpToolAnnotations::new().open_world(false);
    let annotations = match canonical_name {
        "core_search_memories" | "core_memory_spaces" => base.read_only(true),

        "core_derive" => base.read_only(false).destructive(false).idempotent(true),

        "core_remember"
        | "core_record_utterance"
        | "core_goal"
        | "core_link"
        | "core_publish_memory" => base.read_only(false).destructive(false).idempotent(false),

        "core_wake" | "core_personality" => {
            base.read_only(false).destructive(true).idempotent(false)
        }

        "core_fact" => base.read_only(false).destructive(true).idempotent(true),

        _ => return None,
    };
    Some(annotations)
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, BTreeSet},
        sync::Arc,
    };

    use super::*;
    use crate::{AuthPath, FlavorRegistry, Principal, UserId};

    #[test]
    fn provider_safe_tool_name_replaces_runner_invalid_separators() {
        assert_eq!(
            provider_safe_tool_name("core/emit_abstraction"),
            "core_emit_abstraction"
        );
        assert_eq!(provider_safe_tool_name("core_remember"), "core_remember");
        assert_eq!(provider_safe_tool_name("a..b"), "a._b");
    }

    #[test]
    fn core_resources_manifest_has_expected_shape() {
        let resources = all_core_resources().collect::<Vec<_>>();

        assert_eq!(resources.len(), 7);
        assert_eq!(
            resources
                .iter()
                .filter(|resource| !resource.is_template)
                .count(),
            4
        );
        assert_eq!(
            resources
                .iter()
                .filter(|resource| resource.is_template)
                .count(),
            3
        );
        assert!(
            resources
                .iter()
                .all(|resource| resource.scope_key.starts_with("resource:"))
        );
    }

    #[test]
    fn protocol_errors_map_to_json_rpc_error_classes() {
        let forbidden = McpToolError::from(crate::error::ProtocolError::forbidden(
            "source ingest denied",
        ));
        assert_eq!(forbidden.kind(), McpToolErrorKind::InvalidRequest);
        assert!(
            forbidden.to_string().contains("source ingest denied"),
            "message: {forbidden}"
        );

        let invalid = McpToolError::from(crate::error::ProtocolError::invalid_argument(
            "fact",
            "expected Fact id",
        ));
        assert_eq!(invalid.kind(), McpToolErrorKind::InvalidInput);
        assert!(
            invalid.to_string().contains("expected Fact id"),
            "message: {invalid}"
        );
    }

    #[test]
    fn internal_tool_error_client_message_is_generic() {
        let storage = McpToolError::from(crate::StorageError::Internal(
            "postgres password=secret".into(),
        ));
        assert_eq!(storage.kind(), McpToolErrorKind::Internal);
        assert_eq!(storage.client_message(), "internal server error");

        let invalid = McpToolError::InvalidInput("expected Fact id".into());
        assert_eq!(invalid.client_message(), "invalid input: expected Fact id");
    }

    #[test]
    fn core_actions_manifest_is_internally_consistent() {
        let allowed_tools =
            BTreeSet::from(["core_goal", "core_wake", "core_personality", "core_fact"]);
        let expected_counts = BTreeMap::from([
            ("core_goal", 5_usize),
            ("core_wake", 5),
            ("core_personality", 6),
            ("core_fact", 4),
        ]);
        let mut seen_scope_keys = BTreeSet::new();
        let mut counts = BTreeMap::<&'static str, usize>::new();

        for meta in all_core_actions() {
            assert!(
                seen_scope_keys.insert(meta.scope_key),
                "duplicate scope_key {}",
                meta.scope_key
            );
            assert_eq!(
                meta.scope_key,
                format!("{}:{}", meta.tool, meta.action),
                "scope_key must equal <tool>:<action>"
            );
            assert!(
                allowed_tools.contains(meta.tool),
                "unexpected tool {}",
                meta.tool
            );
            *counts.entry(meta.tool).or_default() += 1;
        }

        assert_eq!(counts, expected_counts);
    }

    #[tokio::test]
    async fn dispatcher_rejects_cross_action_goal_fields_before_execution() {
        let ctx = prefixed_ctx();
        let goal = ctx.format_goal(GoalId::new(uuid::Uuid::now_v7()));
        let desc = ctx
            .registry
            .list_mcp_tools()
            .iter()
            .find(|tool| tool.name == "core_goal")
            .expect("core_goal registered");

        let err = (desc.call)(
            ctx,
            serde_json::json!({
                "action": "transition",
                "goal": goal,
                "transition": "pause",
                "title": "belongs to set/modify",
            }),
        )
        .await
        .expect_err("foreign action field must be rejected before execution");
        assert_eq!(err.kind(), McpToolErrorKind::InvalidInput);
        let message = err.to_string();
        assert!(message.contains("title"), "message: {message}");
        assert!(message.contains("transition"), "message: {message}");
    }

    #[tokio::test]
    async fn dispatcher_rejects_cross_action_fact_fields_before_execution() {
        let ctx = prefixed_ctx();
        let fact = ctx.format_fact_memory(MemoryId::new(uuid::Uuid::now_v7()));
        let desc = ctx
            .registry
            .list_mcp_tools()
            .iter()
            .find(|tool| tool.name == "core_fact")
            .expect("core_fact registered");

        let err = (desc.call)(
            ctx,
            serde_json::json!({
                "action": "citation_of_fact",
                "fact": fact,
                "confirm": true,
                "expect_handle": "F:wrong",
            }),
        )
        .await
        .expect_err("foreign action fields must be rejected before execution");
        assert_eq!(err.kind(), McpToolErrorKind::InvalidInput);
        let message = err.to_string();
        assert!(message.contains("confirm"), "message: {message}");
        assert!(message.contains("expect_handle"), "message: {message}");
        assert!(message.contains("citation_of_fact"), "message: {message}");
    }

    #[tokio::test]
    async fn prefixed_ids_round_trip_through_ctx_helpers() {
        let ctx = prefixed_ctx();
        let fact = MemoryId::new(uuid::Uuid::now_v7());
        let abstraction = MemoryId::new(uuid::Uuid::now_v7());
        let perspective = MemoryId::new(uuid::Uuid::now_v7());
        let goal = GoalId::new(uuid::Uuid::now_v7());
        let personality = PersonalityInstanceId::new(uuid::Uuid::now_v7());
        let edge = EdgeId::new(uuid::Uuid::now_v7());
        let wake = uuid::Uuid::now_v7();

        let fact_ref = ctx.format_fact_memory(fact);
        let abstraction_ref = ctx.format_abstraction_memory(abstraction);
        let perspective_ref = ctx.format_perspective_memory(perspective);
        let goal_ref = ctx.format_goal(goal);
        let personality_ref = ctx.format_personality(personality);
        let edge_ref = ctx.format_edge(edge);
        let wake_ref = ctx.format_wake_entry(wake);

        assert_prefixed_uuid(&fact_ref, 'F');
        assert_prefixed_uuid(&abstraction_ref, 'A');
        assert_prefixed_uuid(&perspective_ref, 'P');
        assert_prefixed_uuid(&goal_ref, 'G');
        assert_prefixed_uuid(&personality_ref, 'I');
        assert_prefixed_uuid(&edge_ref, 'E');
        assert_prefixed_uuid(&wake_ref, 'W');

        assert_eq!(ctx.resolve_fact_memory(&fact_ref).expect("fact"), fact);
        assert_eq!(
            ctx.resolve_abstraction_memory(&abstraction_ref)
                .expect("abstraction"),
            abstraction
        );
        assert_eq!(
            ctx.resolve_perspective_memory(&perspective_ref)
                .expect("perspective"),
            perspective
        );
        assert_eq!(ctx.resolve_memory(&fact_ref).expect("any fact"), fact);
        assert_eq!(
            ctx.resolve_memory(&abstraction_ref)
                .expect("any abstraction"),
            abstraction
        );
        assert_eq!(
            ctx.resolve_memory(&perspective_ref)
                .expect("any perspective"),
            perspective
        );
        assert_eq!(ctx.resolve_goal(&goal_ref).expect("goal"), goal);
        assert_eq!(
            ctx.resolve_personality(&personality_ref)
                .expect("personality"),
            personality
        );
        assert_eq!(ctx.resolve_edge(&edge_ref).expect("edge"), edge);
        assert_eq!(ctx.resolve_wake_entry(&wake_ref).expect("wake"), wake);
    }

    #[tokio::test]
    async fn prefixed_ids_ctx_rejects_wrong_class() {
        let ctx = prefixed_ctx();
        let fact = ctx.format_fact_memory(MemoryId::new(uuid::Uuid::now_v7()));
        let err = ctx.resolve_goal(&fact).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("expected Goal id"), "message: {msg}");
        assert!(msg.contains("got prefix 'F'"), "message: {msg}");
    }

    fn prefixed_ctx() -> McpToolCtx {
        let owner = Principal::User(UserId::new(uuid::Uuid::now_v7()));
        McpToolCtx {
            owner: owner.clone(),
            authz: AuthzContext::single_owner(&owner, AuthPath::System),
            handles: None,
            mode: OutputMode::PrefixedIds,
            registry: Arc::new(FlavorRegistry::new().freeze()),
            author: McpAuthorContext {
                model_id: "t".into(),
                client_name: "t".into(),
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

    fn assert_prefixed_uuid(raw: &str, expected_prefix: char) {
        let (prefix, uuid_part) = raw.split_once(':').expect("prefixed uuid");
        let mut expected = [0; 4];
        assert_eq!(prefix, expected_prefix.encode_utf8(&mut expected));
        uuid::Uuid::parse_str(uuid_part).expect("uuid body");
    }
}

#[cfg(test)]
mod ctx_engine_tests {
    use super::*;
    use crate::AuthPath;
    use crate::{Engine, FlavorRegistry, Principal, UserId};
    use std::sync::Arc;

    #[tokio::test]
    async fn ctx_storage_returns_none_when_engine_unwired() {
        let owner = Principal::User(UserId::new(uuid::Uuid::now_v7()));
        let ctx = McpToolCtx {
            owner: owner.clone(),
            authz: AuthzContext::single_owner(&owner, AuthPath::System),
            handles: Some(Arc::new(HandleTable::new())),
            mode: OutputMode::Handles,
            registry: Arc::new(FlavorRegistry::new().freeze()),
            author: McpAuthorContext {
                model_id: "t".into(),
                client_name: "t".into(),
                client_version: "0".into(),
                personality_instance_id: None,
                caller_self_perspective: None,
            },
            caller_self_perspective: None,
            master_token_id: None,
            extensions: McpToolExtensions::default(),
            engine: None,
        };
        assert!(ctx.storage().is_none());
        assert!(ctx.engine().is_none());
    }

    #[tokio::test]
    async fn ctx_storage_returns_some_when_engine_wired() {
        let owner = Principal::User(UserId::new(uuid::Uuid::now_v7()));
        let engine = Arc::new(Engine::new(FlavorRegistry::new().freeze()));
        let ctx = McpToolCtx {
            owner: owner.clone(),
            authz: AuthzContext::single_owner(&owner, AuthPath::System),
            handles: Some(Arc::new(HandleTable::new())),
            mode: OutputMode::Handles,
            registry: Arc::new(FlavorRegistry::new().freeze()),
            author: McpAuthorContext {
                model_id: "t".into(),
                client_name: "t".into(),
                client_version: "0".into(),
                personality_instance_id: None,
                caller_self_perspective: None,
            },
            caller_self_perspective: None,
            master_token_id: None,
            extensions: McpToolExtensions::default(),
            engine: Some(engine.clone()),
        };
        assert!(ctx.engine().is_some());
        assert!(ctx.storage().is_some());
    }
}
