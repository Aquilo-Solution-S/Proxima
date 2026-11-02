use super::contract::SpiiceContract;
use super::{
    Arc, AuthorizationHook, McpToolDescriptor, OwnerResolver, PayloadKind,
    ProtocolPayloadIngressEntry, RequestBehavior, SchemaCapabilityTags, SchemaId, SchemaInfo,
    ScopeGateBehavior, ScopeKind, SpiiceDescriptor,
};
use crate::FactPayload;

/// Mutable build-time registry of the spiices linked into this host.
/// Spiices push into it during their `register()` call; `freeze` consumes it whole into a
/// `SpiiceRegistryFrozen` via `SpiiceRegistryFrozen::from_registry`.
/// Fields are `pub(crate)` so that constructor can destructure them.
#[derive(Debug)]
pub struct SpiiceRegistry {
    pub(crate) schemas: Vec<SchemaInfo>,
    pub(crate) schema_capability_tags: Vec<SchemaCapabilityTags>,
    pub(crate) protocol_ingress: Vec<ProtocolPayloadIngressEntry>,
    pub(crate) mcp_tools: Vec<McpToolDescriptor>,
    pub(crate) request_behaviors: Vec<Arc<dyn RequestBehavior>>,
    pub(crate) spiices: Vec<SpiiceDescriptor>,
    /// One [`SpiiceContract`] per linked spiice. This is what makes erase,
    /// export, forget, transfer and the migration guardrail a registry walk
    /// instead of six hand-maintained lists.
    pub(crate) contracts: Vec<&'static SpiiceContract>,
    /// Every registered payload that named a [`ScopeKind`], paired with the
    /// schema that named it.
    ///
    /// Read off the payload TYPE at registration (`P::SCOPE_KIND`), which is
    /// the only moment core holds the concrete type. Freeze uses it to
    /// refuse a kind no linked contract declares — the schema id travels
    /// with it so the refusal names the registration, not just the kind.
    pub(crate) scoped_schemas: Vec<(SchemaId, ScopeKind)>,
    pub(crate) owner_resolver: Option<Arc<dyn OwnerResolver>>,
    pub(crate) authorization_hooks: Vec<Arc<dyn AuthorizationHook>>,
}

impl Default for SpiiceRegistry {
    fn default() -> Self {
        let mut registry = Self {
            schemas: Vec::new(),
            schema_capability_tags: Vec::new(),
            protocol_ingress: Vec::new(),
            mcp_tools: Vec::new(),
            request_behaviors: vec![Arc::new(ScopeGateBehavior)],
            spiices: Vec::new(),
            contracts: Vec::new(),
            scoped_schemas: Vec::new(),
            owner_resolver: None,
            authorization_hooks: Vec::new(),
        };
        registry
            .try_add_cited_object_schema::<crate::citations::UploadedBlobPayload>()
            .expect("built-in cited-object schema registration must be valid");
        crate::memory::register_all(&mut registry)
            .expect("built-in memory registration must be valid");
        crate::goal::register_all(&mut registry).expect("built-in goal registration must be valid");
        crate::mcp::core_tools::register_all(&mut registry)
            .expect("built-in MCP tool registration must be valid");
        // Spiice #0's contract lands beside the registrations it describes:
        // `default()` is the single entry point, so a binary cannot link
        // core's registrations without its declaration.
        crate::spiice::spiice0::register(&mut registry)
            .expect("spiice #0 contract registration must be valid");
        registry
    }
}

impl SpiiceRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Make `core/goal-change-v1` listenable, so every goal write also writes
    /// its change Fact into the outbox.
    ///
    /// Public only because the runtime composes the registry from another
    /// crate: `LISTENABLE` is a type constant, and whether anything consumes
    /// the outbox is a runtime fact (`RuntimeConfig.outbox_consumers`).
    #[doc(hidden)]
    pub fn publish_goal_changes(&mut self) {
        let goal_change = crate::GoalChangeV1::schema_id();
        for schema in &mut self.schemas {
            if schema.schema_id == goal_change && schema.kind == PayloadKind::Fact {
                schema.listenable = true;
            }
        }
    }
}
