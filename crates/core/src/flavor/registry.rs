use super::contract::FlavorContract;
use super::{
    Arc, AuthorizationHook, FlavorDescriptor, McpToolDescriptor, MemorySearchProjection,
    OwnerResolver, ProtocolPayloadIngressEntry, RequestBehavior, SchemaCapabilityTags, SchemaInfo,
    ScopeGateBehavior,
};

/// Mutable build-time registry. Flavors push into it during their
/// `register()` call; `freeze` consumes it whole into a
/// `FlavorRegistryFrozen` via `FlavorRegistryFrozen::from_registry`.
/// Fields are `pub(crate)` so that constructor can destructure them.
#[derive(Debug)]
pub struct FlavorRegistry {
    pub(crate) schemas: Vec<SchemaInfo>,
    pub(crate) schema_capability_tags: Vec<SchemaCapabilityTags>,
    pub(crate) search_projections: Vec<MemorySearchProjection>,
    pub(crate) protocol_ingress: Vec<ProtocolPayloadIngressEntry>,
    pub(crate) mcp_tools: Vec<McpToolDescriptor>,
    pub(crate) request_behaviors: Vec<Arc<dyn RequestBehavior>>,
    pub(crate) flavors: Vec<FlavorDescriptor>,
    /// One [`FlavorContract`] per linked flavor. This is what makes erase,
    /// export, forget, transfer and the migration guardrail a registry walk
    /// instead of six hand-maintained lists.
    pub(crate) contracts: Vec<&'static FlavorContract>,
    pub(crate) owner_resolver: Option<Arc<dyn OwnerResolver>>,
    pub(crate) authorization_hooks: Vec<Arc<dyn AuthorizationHook>>,
}

impl Default for FlavorRegistry {
    fn default() -> Self {
        let mut registry = Self {
            schemas: Vec::new(),
            schema_capability_tags: Vec::new(),
            search_projections: Vec::new(),
            protocol_ingress: Vec::new(),
            mcp_tools: Vec::new(),
            request_behaviors: vec![Arc::new(ScopeGateBehavior)],
            flavors: Vec::new(),
            contracts: Vec::new(),
            owner_resolver: None,
            authorization_hooks: Vec::new(),
        };
        registry
            .try_add_cited_object_schema::<crate::citations::UploadedBlobPayload>()
            .expect("built-in cited-object schema registration must be valid");
        // Without these two, `core/uploaded-blob-v1` is a cited object no
        // Fact can reach: a mapping is the only path, and the engine
        // requires one whose `cited_object_schema()` names it.
        registry
            .try_add_citation_mapping_schema::<crate::citations::UploadedBlobWholeV1>()
            .expect("built-in whole-blob citation-mapping registration must be valid");
        registry
            .try_add_citation_mapping_schema::<crate::citations::UploadedBlobPageSpanV1>()
            .expect("built-in page-span citation-mapping registration must be valid");
        registry
            .try_add_fact_schema::<crate::verbs::persist_mcp_call::McpCallLoggedV1>()
            .expect("built-in MCP call fact schema registration must be valid");
        registry
            .try_add_cited_object_schema::<crate::verbs::persist_mcp_call::McpCallIoV1>()
            .expect("built-in MCP call cited-object schema registration must be valid");
        registry
            .try_add_citation_mapping_schema::<crate::verbs::persist_mcp_call::McpCallIoCitationV1>(
            )
            .expect("built-in MCP call citation-mapping schema registration must be valid");
        crate::memory::register_all(&mut registry)
            .expect("built-in memory registration must be valid");
        crate::goal::register_all(&mut registry).expect("built-in goal registration must be valid");
        crate::mcp::core_tools::register_all(&mut registry)
            .expect("built-in MCP tool registration must be valid");
        registry
    }
}

impl FlavorRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}
