#[test]
fn host_api_imports_from_root() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<proxima::ComplianceEraseCounts>();
    assert_send_sync::<proxima::ComplianceEraseOutcome>();
    assert_send_sync::<proxima::ComplianceEraseRefusal>();
    assert_send_sync::<proxima::ComplianceEraseRequest>();
    assert_send_sync::<proxima::ComplianceEraseTarget>();
    assert_send_sync::<proxima::CancellationToken>();
    assert_send_sync::<proxima::RuntimeBuilder>();
    assert_send_sync::<proxima::RuntimeConfig>();
    assert_send_sync::<proxima::Engine>();
    let owner: proxima::Owner = proxima::company_owner(uuid::Uuid::nil());
    let _authz: proxima::AuthzContext = proxima::AuthzContext::denied_for_owner(&owner);
    let _narrowed = proxima::AuthzContext::denied_for_owner(&owner).narrowed_to_owner(owner);
    let _cursor: proxima::Cursor = proxima::Cursor::empty();
    let _cancel = proxima::CancellationToken::new();
    std::hint::black_box(proxima::load_source_cursor);
    std::hint::black_box(proxima::store_source_cursor);
    let _outcome = proxima::ComplianceEraseOutcome::Refused {
        operation_id: uuid::Uuid::nil(),
        reason: proxima::ComplianceEraseRefusal::WorldOwner,
    };
}

#[test]
fn flavor_sdk_imports_from_flavor_module() {
    use proxima::flavor::{FactPayload, FlavorBundle, FlavorRegistry, PgSidecarRegistry, SchemaId};
    fn _needs_bundle<T: FlavorBundle>() {}
    fn _needs_fact<T: FactPayload>() {}
    let _ = SchemaId::new("test/schema-v1".to_owned());
    let _ = FlavorRegistry::new();
    let _ = PgSidecarRegistry::new();
}

#[test]
fn flavor_sdk_exposes_mcp_tool_authoring_surface() {
    // The MCP tool family is reachable from `proxima::flavor` so flavor
    // authors never import `proxima_core::mcp` directly.
    use proxima::flavor::{
        McpActionArgSpec, McpAuthorContext, McpTool, McpToolAnnotations, McpToolCtx, McpToolError,
        McpToolErrorKind,
    };
    fn _needs_mcp_tool<T: McpTool>() {}
    let _ = McpToolErrorKind::Internal;
    // Name the remaining re-exports as types so an accidental removal fails.
    let _: &[McpActionArgSpec] = &[];
    let _: Option<(
        &McpToolCtx,
        &McpToolError,
        &McpAuthorContext,
        &McpToolAnnotations,
    )> = None;
}

#[test]
fn flavor_sdk_exposes_the_cited_blob_lane() {
    // `FlavorWorkerContext::blobs` is a public field, so its type and the
    // trait behind it must be nameable from `proxima::flavor` alone — a
    // flavor depending only on `proxima` cannot reach into
    // `proxima_core::storage_ports`.
    use proxima::flavor::{
        CitedBlobPort, CitedBlobReadUrl, CitedBlobService, CitedBlobUploadAborted,
        CitedBlobUploadCompleted, CitedBlobUploadHeader, CitedBlobUploadPrepared,
    };
    fn _needs_port<T: CitedBlobPort>() {}
    let _: Option<(
        &CitedBlobService,
        &CitedBlobReadUrl,
        &CitedBlobUploadPrepared,
        &CitedBlobUploadCompleted,
        &CitedBlobUploadAborted,
        &CitedBlobUploadHeader,
    )> = None;
}

#[test]
fn host_api_exposes_role_for_worker_authorization() {
    // A flavor worker mints its own `AuthzContext` per job, and
    // `for_subject_with_role` is the only mint that works for a group
    // owner — which `company_owner` produces. Without `Role` on the host
    // facade its parameter type is unnameable.
    let owner: proxima::Owner = proxima::company_owner(uuid::Uuid::nil());
    let _authz: proxima::AuthzContext = proxima::AuthzContext::for_subject_with_role(
        proxima::UserId::new(uuid::Uuid::nil()),
        [(owner, proxima::Role::admin())],
        proxima::AuthPath::System,
    );
}

/// A flavor-owned Abstraction, defined through the SDK alone. Note
/// `sidecar_table` is required rather than optional on this trait: unlike
/// a Fact, a derived memory always has a typed sidecar, so a flavor
/// registering one always owns a migration for it too.
#[derive(serde::Serialize, serde::Deserialize)]
struct TierAbstraction {
    note: String,
}

impl proxima::flavor::AbstractionPayload for TierAbstraction {
    const SCHEMA_ID: &'static str = "proxima-tier/abstraction-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_tier.abstraction_v1"
    }
}

#[test]
fn flavor_sdk_exposes_the_derived_memory_write_lane() {
    // `AbstractionPayload` and `PerspectivePayload` let a flavor *declare*
    // derived schemas; without these types it could never *write* one,
    // because `Engine::author_derived_authorized` takes an
    // `AuthorDerivedRequestInput` an out-of-tree flavor could not name.
    // The in-tree precedent (`flavors/code`) reaches the same lane through
    // a direct `proxima-storage-pg` dependency, which a flavor depending
    // only on `proxima` does not have.
    //
    // This constructs the request rather than merely naming the types, so
    // a field added, removed or retyped upstream breaks here instead of
    // silently breaking every out-of-tree flavor at its next pin bump.
    use proxima::flavor::{
        AbstractionPayload, AuthorDerivedEdgeInput, AuthorDerivedRequestInput,
        CORE_DERIVED_FROM_RELATION, EdgeAuthorshipKind, EntityKind, FlavorRegistry,
        InputContractId, MemoryId, MemoryOperatorKind, OperatorId, SchemaVersion, SidecarPayload,
    };

    let frozen = FlavorRegistry::new()
        .try_freeze()
        .expect("an empty registry freezes");
    let relation = frozen
        .resolve_relation(CORE_DERIVED_FROM_RELATION)
        .expect("core/derived-from is a substrate relation, present without any flavor");

    let owner: proxima::Owner = proxima::company_owner(uuid::Uuid::nil());
    let derived = MemoryId::new(uuid::Uuid::nil());
    let source_fact = MemoryId::new(uuid::Uuid::nil());

    let edges = [AuthorDerivedEdgeInput {
        relation,
        source_kind: EntityKind::Abstraction,
        source_memory_id: derived,
        target_kind: EntityKind::Fact,
        target_memory_id: source_fact,
        authorship_kind: EdgeAuthorshipKind::OperatorFtoA,
        authorship_owner_memory_id: None,
    }];

    let _req = AuthorDerivedRequestInput {
        memory_id: derived,
        owner,
        kind: EntityKind::Abstraction,
        text: "the text a derived memory is embedded from".to_owned(),
        schema_id: <TierAbstraction as proxima::flavor::AbstractionPayload>::schema_id(),
        schema_version: SchemaVersion::new(TierAbstraction::SCHEMA_VERSION),
        operator_kind: MemoryOperatorKind::FtoA,
        operator_id: OperatorId::new(uuid::Uuid::nil()),
        input_contract_id: InputContractId::new(uuid::Uuid::nil()),
        source_batch_id: None,
        model_id: "tier-test",
        prompt_version: "1",
        sidecar_payload: SidecarPayload::abstraction(TierAbstraction {
            note: "sidecar".to_owned(),
        }),
        supersedes: None,
        lexical_language: None,
        edges: &edges,
    };

    // The outcome type must be nameable too — a caller has to bind what
    // `author_derived_authorized` returns.
    let _: Option<&proxima::flavor::AuthorDerivedAuthorizedOutcome> = None;
    // `supersedes` above is the re-derivation path; naming the relation it
    // writes keeps that half of the contract pinned as well.
    assert_ne!(
        proxima::flavor::CORE_SUPERSEDES_RELATION,
        CORE_DERIVED_FROM_RELATION
    );
}

/// A flavor-owned service, standing in for the store a real flavor hands
/// its tools (`proxima-code` puts a `CodeFlavorStore` here). Core cannot
/// name this type, which is the entire point of the extension map.
struct TierFlavorStore {
    marker: &'static str,
}

/// The override a flavor host must be able to *write*. This is the load-
/// bearing half: `mcp_tool_extensions` returns `McpToolExtensions`, so
/// without that type on the flavor facade the signature is unspellable and
/// a flavor depending only on `proxima` cannot supply its tools with a
/// database handle or any other host-owned dependency.
struct TierExtensionApp;

impl proxima::flavor::FlavorBundle for TierExtensionApp {
    fn register(
        _registry: &mut proxima::flavor::FlavorRegistry,
    ) -> Result<(), proxima::flavor::FlavorRegistryError> {
        Ok(())
    }

    fn migrators() -> Vec<proxima::flavor::NamedMigrator> {
        Vec::new()
    }
}

impl proxima::FlavorApp for TierExtensionApp {
    fn app_info() -> proxima::AppInfo {
        proxima::AppInfo {
            id: "proxima-tier",
            title: "Tier Extension App",
            version: "0",
        }
    }

    fn mcp_tool_extensions(ctx: &proxima::AppContext) -> proxima::flavor::McpToolExtensions {
        // A real host composes its store from `ctx.clone_pool_for_host()`.
        // The pool stays off the supported tier deliberately (see
        // `raw_storage_surfaces_are_not_supported_tier_exports`), so this
        // test only pins that the override is writable at all.
        let _ = ctx;
        let mut extensions = proxima::flavor::McpToolExtensions::default();
        extensions.insert(TierFlavorStore { marker: "store" });
        extensions
    }
}

#[test]
fn flavor_sdk_exposes_the_mcp_tool_extension_seam() {
    use proxima::flavor::McpToolExtensions;

    // Both halves must be reachable: the host inserts a service core
    // cannot name, and the tool resolves it back by type.
    let mut extensions = McpToolExtensions::default();
    extensions.insert(TierFlavorStore { marker: "store" });
    let resolved = extensions
        .get::<TierFlavorStore>()
        .expect("a tool must resolve the service its host inserted");
    assert_eq!(resolved.marker, "store");

    // The one-shot constructor is the common case for a single service.
    let single = McpToolExtensions::with(TierFlavorStore { marker: "single" });
    assert_eq!(
        single
            .get::<TierFlavorStore>()
            .expect("with() inserts the value")
            .marker,
        "single"
    );

    // An absent service resolves to None rather than panicking — the
    // degraded mode every extension-resolving tool has to handle.
    assert!(single.get::<u64>().is_none());

    // And the override itself is nameable; calling it needs an AppContext
    // only a booted runtime can make, so this pins the signature.
    std::hint::black_box(
        <TierExtensionApp as proxima::FlavorApp>::mcp_tool_extensions
            as fn(&proxima::AppContext) -> McpToolExtensions,
    );
}

#[test]
fn raw_storage_surfaces_are_not_supported_tier_exports() {
    let host_exports = include_str!("../src/host.rs");
    let flavor_exports = include_str!("../src/flavor.rs");

    assert!(!host_exports.contains("PgPool"));
    assert!(!host_exports.contains("PgStorage"));
    assert!(!host_exports.contains("StorageHandle"));
    assert!(!flavor_exports.contains("PgPool"));
    assert!(!flavor_exports.contains("PgStorage"));
    assert!(!flavor_exports.contains("StorageHandle"));
}
