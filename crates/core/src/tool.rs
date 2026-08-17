//! Transport-neutral flavor tool SDK.

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::Arc;

use futures::future::BoxFuture;

use crate::access::AccessKind;
use crate::storage_ports::OwnerWritePermit;
use crate::text_bounds::check_trimmed_len;
use crate::{AuthzContext, Engine, FlavorRegistryFrozen, MemoryId, Owner};

#[derive(Clone)]
struct ServiceEntry {
    type_name: &'static str,
    value: Arc<dyn Any + Send + Sync>,
}

type ServiceValues = Arc<HashMap<TypeId, ServiceEntry>>;

/// One immutable-at-runtime service set composed by the linked flavors.
///
/// Values are keyed by their concrete Rust type. Composition is fallible:
/// two linked flavors cannot silently replace one another's service, and a
/// flavor cannot replace a substrate service such as `CitedBlobService`.
#[derive(Clone, Default)]
pub struct FlavorServices {
    values: ServiceValues,
}

impl std::fmt::Debug for FlavorServices {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FlavorServices")
            .field("len", &self.values.len())
            .finish_non_exhaustive()
    }
}

/// Failure to compose the runtime services of linked flavors.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FlavorServiceError {
    #[error("duplicate flavor service type `{type_name}`")]
    DuplicateService { type_name: &'static str },
}

impl FlavorServices {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with<T>(value: T) -> Self
    where
        T: Send + Sync + 'static,
    {
        Self {
            values: Arc::new(HashMap::from([(
                TypeId::of::<T>(),
                ServiceEntry {
                    type_name: std::any::type_name::<T>(),
                    value: Arc::new(value),
                },
            )])),
        }
    }

    /// Insert one service without replacing an existing service of the same
    /// concrete type.
    ///
    /// # Errors
    ///
    /// Returns [`FlavorServiceError::DuplicateService`] when the type is
    /// already present. The set is unchanged on failure.
    pub fn try_insert<T>(&mut self, value: T) -> Result<(), FlavorServiceError>
    where
        T: Send + Sync + 'static,
    {
        let type_id = TypeId::of::<T>();
        if self.values.contains_key(&type_id) {
            return Err(FlavorServiceError::DuplicateService {
                type_name: std::any::type_name::<T>(),
            });
        }
        Arc::make_mut(&mut self.values).insert(
            type_id,
            ServiceEntry {
                type_name: std::any::type_name::<T>(),
                value: Arc::new(value),
            },
        );
        Ok(())
    }

    /// Merge another composed set without overriding either side.
    ///
    /// # Errors
    ///
    /// Returns the lexically first duplicate concrete type. The receiver is
    /// unchanged on failure.
    pub fn try_extend(&mut self, other: Self) -> Result<(), FlavorServiceError> {
        let duplicate = other
            .values
            .iter()
            .filter(|(type_id, _)| self.values.contains_key(type_id))
            .map(|(_, entry)| entry.type_name)
            .min();
        if let Some(type_name) = duplicate {
            return Err(FlavorServiceError::DuplicateService { type_name });
        }
        let other_values = Arc::unwrap_or_clone(other.values);
        Arc::make_mut(&mut self.values).extend(other_values);
        Ok(())
    }

    #[must_use]
    pub fn get<T>(&self) -> Option<Arc<T>>
    where
        T: Send + Sync + 'static,
    {
        service(&self.values)
    }

    #[must_use]
    pub(crate) fn into_tool_services(self) -> ToolServices {
        ToolServices {
            values: self.values,
        }
    }
}

#[derive(Clone, Default)]
pub struct ToolServices {
    values: ServiceValues,
}

impl std::fmt::Debug for ToolServices {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolServices")
            .field("len", &self.values.len())
            .finish_non_exhaustive()
    }
}

impl ToolServices {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with<T>(value: T) -> Self
    where
        T: Send + Sync + 'static,
    {
        let mut services = Self::default();
        services.insert(value);
        services
    }

    pub fn insert<T>(&mut self, value: T) -> Option<Arc<T>>
    where
        T: Send + Sync + 'static,
    {
        Arc::make_mut(&mut self.values)
            .insert(
                TypeId::of::<T>(),
                ServiceEntry {
                    type_name: std::any::type_name::<T>(),
                    value: Arc::new(value),
                },
            )
            .and_then(|old| old.value.downcast::<T>().ok())
    }

    #[must_use]
    pub fn get<T>(&self) -> Option<Arc<T>>
    where
        T: Send + Sync + 'static,
    {
        service(&self.values)
    }
}

fn service<T>(values: &ServiceValues) -> Option<Arc<T>>
where
    T: Send + Sync + 'static,
{
    values
        .get(&TypeId::of::<T>())
        .map(|entry| entry.value.clone())
        .and_then(|value| value.downcast::<T>().ok())
}

/// Transport-neutral identity of the client invoking a generic tool.
///
/// Transport adapters populate this from their authenticated call context.
/// Direct hosts may omit it when caller provenance is unavailable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCaller {
    pub model_id: String,
    pub client_name: String,
    pub client_version: String,
}

impl ToolCaller {
    #[must_use]
    pub fn new(
        model_id: impl Into<String>,
        client_name: impl Into<String>,
        client_version: impl Into<String>,
    ) -> Self {
        Self {
            model_id: model_id.into(),
            client_name: client_name.into(),
            client_version: client_version.into(),
        }
    }
}

#[derive(Clone)]
pub struct ToolCtx {
    owner: Owner,
    authz: AuthzContext,
    registry: Arc<FlavorRegistryFrozen>,
    caller: Option<ToolCaller>,
    caller_self_perspective: Option<MemoryId>,
    services: ToolServices,
    engine: Option<Arc<Engine>>,
}

impl std::fmt::Debug for ToolCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolCtx")
            .field("owner", &self.owner)
            .field("caller", &self.caller)
            .field("caller_self_perspective", &self.caller_self_perspective)
            .field("services", &self.services)
            .field("has_engine", &self.engine.is_some())
            .finish_non_exhaustive()
    }
}

impl ToolCtx {
    #[must_use]
    pub fn new(
        owner: Owner,
        authz: AuthzContext,
        registry: Arc<FlavorRegistryFrozen>,
        services: ToolServices,
    ) -> Self {
        Self {
            owner,
            authz,
            registry,
            caller: None,
            caller_self_perspective: None,
            services,
            engine: None,
        }
    }

    /// Attach transport-neutral caller provenance.
    #[must_use]
    pub fn with_caller(mut self, caller: Option<ToolCaller>) -> Self {
        self.caller = caller;
        self
    }

    #[must_use]
    pub fn with_caller_self_perspective(mut self, memory_id: Option<MemoryId>) -> Self {
        self.caller_self_perspective = memory_id;
        self
    }

    #[must_use]
    pub fn with_engine(mut self, engine: Option<Arc<Engine>>) -> Self {
        self.engine = engine;
        self
    }

    #[must_use]
    pub(crate) fn from_parts(
        owner: Owner,
        authz: AuthzContext,
        registry: Arc<FlavorRegistryFrozen>,
        caller: Option<ToolCaller>,
        caller_self_perspective: Option<MemoryId>,
        services: ToolServices,
        engine: Option<Arc<Engine>>,
    ) -> Self {
        Self {
            owner,
            authz,
            registry,
            caller,
            caller_self_perspective,
            services,
            engine,
        }
    }

    #[must_use]
    pub fn owner(&self) -> Owner {
        self.owner
    }

    #[must_use]
    pub fn authz(&self) -> &AuthzContext {
        &self.authz
    }

    #[must_use]
    pub fn registry(&self) -> &FlavorRegistryFrozen {
        &self.registry
    }

    #[must_use]
    pub fn caller(&self) -> Option<&ToolCaller> {
        self.caller.as_ref()
    }

    #[must_use]
    pub fn caller_self_perspective(&self) -> Option<MemoryId> {
        self.caller_self_perspective
    }

    #[must_use]
    pub fn engine(&self) -> Option<Arc<Engine>> {
        self.engine.clone()
    }

    /// Authorize this tool context for a storage-tier owner write.
    ///
    /// The permit is minted by the engine from this context's real transport
    /// authorization and scoped owner; flavor code cannot construct it.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::Other`] when the tool was not wired with an engine
    /// and [`ToolError::Protocol`] when authorization fails.
    pub async fn owner_write_permit(
        &self,
        kind: AccessKind,
    ) -> Result<OwnerWritePermit, ToolError> {
        let engine = self
            .engine
            .as_ref()
            .ok_or_else(|| ToolError::Other("tool context has no engine".into()))?;
        engine
            .authorize_owner_write(&self.authz, &self.owner, kind)
            .await
            .map_err(ToolError::Protocol)
    }

    #[must_use]
    pub fn service<T: Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        self.services.get::<T>()
    }
}

/// Reject `limit: 0` on any paged read. `None` means the caller omitted
/// the bound and takes the tool's default, which is always fine.
///
/// The two ends of a page bound are not symmetric. A limit *above* the
/// maximum can be clamped, because "as many as you will give me" is still
/// the caller's intent and the page they get answers it. Zero answers
/// nothing: it yields a well-formed empty page that no client can tell
/// apart from "nothing matched", or — worse — a clamped page of one, which
/// answers a question that was not asked.
///
/// This lives on `ToolError` rather than `McpToolError` so one
/// implementation serves both tool traits: `From<ToolError> for
/// McpToolError` maps `InvalidInput` straight through, so an
/// `McpTool` body can `?` this directly. It is `pub` because every flavor
/// with a paged read needs the same rule, and a rule an out-of-tree flavor
/// cannot reach is a rule it will reimplement differently.
///
/// The engine has enforced this from the start (`engine::query`,
/// `engine::read_verbs`); this is the tool layer agreeing with it instead
/// of clamping first and hiding it.
///
/// # Errors
///
/// [`ToolError::InvalidInput`] when `limit` is `Some(0)`.
pub fn reject_zero_limit(limit: Option<u32>) -> Result<(), ToolError> {
    if limit == Some(0) {
        return Err(ToolError::InvalidInput("limit must be at least 1".into()));
    }
    Ok(())
}

/// Longest search query any tool accepts, in characters.
///
/// One number because the arguments for it are not tool-specific: it
/// bounds the tsquery the lexical arm builds and the token count the
/// embedding arm pays for, and a caller who has to remember a different
/// cap per tool will get it wrong.
pub const MAX_QUERY_CHARS: usize = 512;

/// Longest text cap a caller may ask a search result to carry, in
/// characters — `core_search_memories`' `body_max_chars`,
/// `proxima-code_search_chunks`' `snippet_max_chars`, and whatever a
/// flavor names its own.
///
/// Shared rather than copied because the two in-tree values were already
/// the same number by intent (the code flavor's comment said "matching
/// `core_search_memories`") and nothing would have caught them drifting
/// apart. Defaults are deliberately NOT shared: 2,000 for a code chunk and
/// 8,000 for a memory body are different because the objects are.
pub const MAX_TEXT_CAP_CHARS: usize = 8_000;

/// Trim `value` and check its length against `1..=max` characters,
/// returning the trimmed text and naming the bound that was actually
/// broken.
///
/// Blank and oversized are two different mistakes with two different
/// fixes, and a caller can only act on the one they made. A single
/// `body must be 1..=20000 chars` names a range a blank value satisfies,
/// which reads as a server fault rather than an instruction to send
/// content.
///
/// Counts characters, not bytes, for the reason
/// [`validate_search_query`] does: a cap that bound bytes would reject a
/// shorter text written in a language that does not fit in ASCII.
///
/// `field` is the wire name of the parameter, so the message points at
/// something the caller can see in the schema.
///
/// This is the tool-SDK spelling of [`check_trimmed_len`]; the rule and its
/// wording live there because `verbs` enforces the same contract on
/// `IdempotencyKey` and goal display fields and cannot reach this module.
///
/// # Errors
///
/// [`ToolError::InvalidInput`] when `value` is empty after trimming, or
/// longer than `max` characters.
pub fn validate_trimmed_len<'a>(
    field: &str,
    value: &'a str,
    max: usize,
) -> Result<&'a str, ToolError> {
    check_trimmed_len(value, max)
        .map_err(|violation| ToolError::InvalidInput(violation.reason(field)))
}

/// Trim `query` and check it against [`MAX_QUERY_CHARS`].
///
/// Three tools carried a byte-identical copy of this with `512` inlined
/// as a literal in each — see [`reject_zero_limit`] for why a rule with no
/// shared home is a rule that eventually disagrees with itself.
///
/// # Errors
///
/// [`ToolError::InvalidInput`] when the trimmed query is empty or longer
/// than [`MAX_QUERY_CHARS`].
pub fn validate_search_query(query: &str) -> Result<&str, ToolError> {
    validate_trimmed_len("query", query, MAX_QUERY_CHARS)
}

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("invalid input: {0}")]
    InvalidInput(String),
    /// Well-formed reference to an entity that does not exist or is not
    /// visible. Maps to [`crate::mcp::McpToolError::NotFound`] (REST 404).
    #[error("{0}")]
    NotFound(String),
    #[error("tool not authorized: {0}")]
    NotAuthorized(String),
    #[error("{0}")]
    Protocol(#[from] crate::error::ProtocolError),
    #[error("layering violation: {0}")]
    LayeringViolation(String),
    #[error("storage: {0}")]
    Storage(#[from] crate::StorageError),
    /// A required capability is not configured (embedding client, blob
    /// lane, engine). Maps to [`crate::mcp::McpToolError::Unavailable`]
    /// (REST 503), not a redacted 500.
    #[error("{0}")]
    Unavailable(String),
    #[error("{0}")]
    Other(String),
}

/// A tool written against typed arguments and a typed answer, with no wire
/// concepts in its signature.
///
/// There is deliberately no transport-neutral descriptor beside this trait.
/// The blanket `impl<T: Tool> McpTool for T` adapts the context, and
/// registration mints exactly one
/// [`McpToolDescriptor`](crate::mcp::McpToolDescriptor) per tool — which is
/// what the scope gate, the tool catalog, the REST action routes and the
/// `OpenAPI` document all read. A second descriptor type would be a second
/// answer to "what is registered", and the one no seam consults is the one
/// that drifts.
pub trait Tool: Send + Sync + 'static {
    const NAME: &'static str;
    const DESCRIPTION: &'static str;
    const PRODUCES_SCHEMA_IDS: &'static [&'static str] = &[];
    /// MCP behaviour hints for a flat tool.
    ///
    /// Not cosmetic. `ScopeGateBehavior::enforce_owner_role` asks whether
    /// a flat tool is read-only and demands WRITE access when it cannot tell,
    /// so a flat read that declares nothing is refused to every read-only
    /// role. Dispatchers ignore this parent declaration and resolve each
    /// action only from [`Self::ACTION_ARG_SPECS`].
    const ANNOTATIONS: Option<crate::mcp::McpToolAnnotations> = None;
    /// The actions this tool dispatches, or `&[]` for a flat tool.
    ///
    /// This is THE enumeration of a dispatcher's action set — the scope
    /// gate, the tool catalog, the REST action routes, and the `OpenAPI`
    /// document all read it off `McpToolDescriptor::action_arg_specs`.
    /// Declaring it turns a tool into a dispatcher: its `Args` must be an
    /// internally tagged enum tagged on `action`, its arguments are
    /// validated per action before decode, and its scope keys become
    /// `tool:action` leaves rather than the bare tool name.
    /// `FlavorRegistry::try_freeze` refuses a registry where these and the
    /// schemars-derived `x-proxima-actions` disagree. Each spec's annotations
    /// are the sole read/write authority for that action; missing means write.
    const ACTION_ARG_SPECS: &'static [crate::mcp::McpActionArgSpec] = &[];

    type Args: serde::de::DeserializeOwned + schemars::JsonSchema + Send + 'static;
    /// What the tool answers with. `JsonSchema` is required for the same
    /// reason it is on `Args`: the manifest describes both ends of the call,
    /// and the derived schema is what MCP clients validate
    /// `structuredContent` against.
    type Output: serde::Serialize + schemars::JsonSchema + Send + 'static;

    fn call(ctx: ToolCtx, args: Self::Args) -> BoxFuture<'static, Result<Self::Output, ToolError>>;
}

#[cfg(test)]
mod flavor_service_tests {
    use std::sync::Arc;

    use super::{FlavorServiceError, FlavorServices};

    #[derive(Debug)]
    struct Alpha(&'static str);

    #[derive(Debug)]
    struct Beta;

    #[derive(Debug)]
    struct Gamma;

    #[test]
    fn duplicate_insert_is_typed_and_keeps_the_original() {
        let mut services = FlavorServices::with(Alpha("first"));
        let original = services.get::<Alpha>().expect("alpha present");

        let err = services.try_insert(Alpha("second")).unwrap_err();

        assert_eq!(
            err,
            FlavorServiceError::DuplicateService {
                type_name: std::any::type_name::<Alpha>(),
            }
        );
        let retained = services.get::<Alpha>().expect("original retained");
        assert!(Arc::ptr_eq(&original, &retained));
        assert_eq!(retained.0, "first");
    }

    #[test]
    fn duplicate_extend_is_atomic() {
        let mut services = FlavorServices::with(Alpha("first"));
        let mut incoming = FlavorServices::with(Beta);
        incoming.try_insert(Alpha("second")).unwrap();
        incoming.try_insert(Gamma).unwrap();

        let err = services.try_extend(incoming).unwrap_err();

        assert!(matches!(
            err,
            FlavorServiceError::DuplicateService { type_name }
                if type_name == std::any::type_name::<Alpha>()
        ));
        assert!(services.get::<Beta>().is_none());
        assert!(services.get::<Gamma>().is_none());
        assert_eq!(services.get::<Alpha>().expect("alpha retained").0, "first");
    }

    #[test]
    fn tool_services_share_the_composed_service_instances() {
        let services = FlavorServices::with(Alpha("shared"));
        let flavor_handle = services.get::<Alpha>().expect("alpha present");

        let tool_handle = services
            .into_tool_services()
            .get::<Alpha>()
            .expect("alpha reaches tool services");

        assert!(Arc::ptr_eq(&flavor_handle, &tool_handle));
    }
}

#[cfg(test)]
mod shared_arg_rule_tests {
    use super::{MAX_QUERY_CHARS, MAX_TEXT_CAP_CHARS, ToolError, validate_search_query};

    #[test]
    fn a_query_is_trimmed_and_bounded() {
        assert_eq!(validate_search_query("  hello  ").unwrap(), "hello");
        assert_eq!(
            validate_search_query(&"a".repeat(MAX_QUERY_CHARS))
                .unwrap()
                .len(),
            MAX_QUERY_CHARS
        );
        assert!(validate_search_query("").is_err());
        assert!(validate_search_query("   ").is_err());
        assert!(validate_search_query(&"a".repeat(MAX_QUERY_CHARS + 1)).is_err());
    }

    /// Whitespace-only is rejected AFTER trimming, not before: `"   "` is
    /// an empty query.
    #[test]
    fn the_bound_applies_to_the_trimmed_query() {
        let padded = format!("  {}  ", "a".repeat(MAX_QUERY_CHARS));
        assert!(
            validate_search_query(&padded).is_ok(),
            "padding must not push a legal query over the cap"
        );
    }

    /// Characters, not bytes. A byte cap would reject a shorter question
    /// for being written in a language that does not fit in ASCII.
    #[test]
    fn the_cap_counts_characters_not_bytes() {
        let cyrillic = "я".repeat(MAX_QUERY_CHARS);
        assert_eq!(cyrillic.len(), MAX_QUERY_CHARS * 2, "two bytes per char");
        assert!(
            validate_search_query(&cyrillic).is_ok(),
            "a 512-character Russian query is as legal as a 512-character English one"
        );
        assert!(validate_search_query(&"я".repeat(MAX_QUERY_CHARS + 1)).is_err());
    }

    #[test]
    fn the_rejection_is_invalid_input_and_names_the_bound() {
        let ToolError::InvalidInput(message) =
            validate_search_query(&"a".repeat(MAX_QUERY_CHARS + 1)).unwrap_err()
        else {
            panic!("a bad query must be invalid input, not any other error kind");
        };
        assert!(
            message.contains(&MAX_QUERY_CHARS.to_string()),
            "the message must carry the bound: {message}"
        );
    }

    /// Blank and oversized are different mistakes, so they get different
    /// messages. Answering a blank query with `must be 1..=512 chars` names
    /// a range the input satisfies, which reads as a server fault rather
    /// than an instruction to send content.
    #[test]
    fn blank_and_oversized_do_not_share_one_message() {
        let ToolError::InvalidInput(blank) = validate_search_query("   ").unwrap_err() else {
            panic!("blank must be invalid input");
        };
        let ToolError::InvalidInput(over) =
            validate_search_query(&"a".repeat(MAX_QUERY_CHARS + 1)).unwrap_err()
        else {
            panic!("oversized must be invalid input");
        };
        assert_ne!(blank, over, "one message for two mistakes tells neither");
        assert!(
            blank.contains("blank"),
            "a blank query must be told it is blank: {blank}"
        );
        assert!(
            !blank.contains(&MAX_QUERY_CHARS.to_string()),
            "a blank query must not be quoted a length bound it satisfies: {blank}"
        );
    }

    /// The message reports what the caller actually sent, so an agent one
    /// character over does not have to guess how far over it is.
    #[test]
    fn the_oversize_message_reports_the_length_that_was_sent() {
        let ToolError::InvalidInput(message) =
            validate_search_query(&"a".repeat(MAX_QUERY_CHARS + 7)).unwrap_err()
        else {
            panic!("oversized must be invalid input");
        };
        assert!(
            message.contains(&(MAX_QUERY_CHARS + 7).to_string()),
            "the message must carry the length sent: {message}"
        );
    }

    /// The length reported is the trimmed one, matching the length that
    /// was actually measured.
    #[test]
    fn the_reported_length_is_the_trimmed_length() {
        let padded = format!("   {}   ", "a".repeat(MAX_QUERY_CHARS + 1));
        let ToolError::InvalidInput(message) = validate_search_query(&padded).unwrap_err() else {
            panic!("oversized must be invalid input");
        };
        assert!(
            message.contains(&(MAX_QUERY_CHARS + 1).to_string())
                && !message.contains(&padded.chars().count().to_string()),
            "the reported length must be the trimmed one: {message}"
        );
    }

    /// The two in-tree text caps were already the same number by intent;
    /// sharing the constant is what makes that true by construction.
    #[test]
    fn the_shared_text_cap_is_the_number_both_tools_documented() {
        assert_eq!(MAX_TEXT_CAP_CHARS, 8_000);
    }
}
