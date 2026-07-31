use std::num::NonZeroU32;

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
fn host_api_can_construct_every_compliance_erase_target() {
    // `ComplianceEraseTarget` was already on the facade, but two of its
    // five variants take a `GroupId`/`SourceId` the facade did not name,
    // so a host depending on `proxima` alone could not build them. An
    // exported enum whose variants are unconstructible is not exported.
    let group_id = proxima::GroupId::new(uuid::Uuid::nil());
    let source_id = proxima::SourceId::new("proxima-tier/scope/0");
    let user_id = proxima::UserId::new(uuid::Uuid::nil());

    let targets: [proxima::ComplianceEraseTarget; 5] = [
        proxima::ComplianceEraseTarget::WorldOwner,
        proxima::ComplianceEraseTarget::GroupOwner { group_id },
        proxima::ComplianceEraseTarget::PersonalOwner {
            user_id,
            drop_event_id: "drop-1".to_owned(),
        },
        proxima::ComplianceEraseTarget::GroupSourceScope {
            group_id,
            source_id: source_id.clone(),
        },
        proxima::ComplianceEraseTarget::PersonalSourceScope {
            user_id,
            source_id,
            drop_event_id: "drop-1".to_owned(),
        },
    ];

    // Naming the count pins the enum's shape: a sixth variant has to be
    // added here, which is the prompt to check it is constructible too.
    assert_eq!(targets.len(), 5);
}

#[test]
fn host_api_names_every_citation_schema_id() {
    // `CitationSpec::v1` takes `impl Into<String>`, so a missing constant
    // never fails at the call site — it just pushes the caller onto a
    // bare literal that silently stops matching when the constant it
    // duplicates is renamed. Both locator mappings are named here so a
    // rename breaks this test instead of a flavor's citations.
    let whole = proxima::CitationSpec::v1(
        proxima::UPLOADED_BLOB_SCHEMA_ID,
        [0u8; 32],
        proxima::UPLOADED_BLOB_WHOLE_SCHEMA_ID,
    );
    let page_span = proxima::CitationSpec::v1(
        proxima::UPLOADED_BLOB_SCHEMA_ID,
        [0u8; 32],
        proxima::UPLOADED_BLOB_PAGE_SPAN_SCHEMA_ID,
    );
    assert_ne!(whole.mapping_schema_id, page_span.mapping_schema_id);
    assert_eq!(
        whole.cited_object_schema_id,
        page_span.cited_object_schema_id
    );
}

#[test]
fn host_api_can_build_a_non_mistral_embedding_client() {
    // `OpenAiCompatEmbeddingClient` was already exported, but its `new`
    // takes an `EmbedCaps` the facade did not name — so `mistral()` was the
    // only constructible embedding client, and every other
    // OpenAI-compatible endpoint (a local Ollama, a self-hosted server) was
    // out of reach for a host depending on `proxima` alone.
    // Built through the constructor, not a struct literal, and deliberately:
    // a literal names every field, so each new capability axis breaks every
    // out-of-tree host that ever built one. `new` + `with_*` keeps them
    // compiling across a version that adds an axis they do not set.
    let caps = proxima::EmbedCaps::new(
        u32::try_from(proxima::llm::EMBEDDING_DIM).expect("the width fits u32"),
        // The reason a local endpoint needs this at all: a model whose
        // native width is not EMBEDDING_DIM must be asked for a nested
        // prefix, or every write fails the fixed-width vector column.
        true,
    )
    // The other reason a local endpoint needs the facade to name this: a
    // runner that dies on over-long input rather than refusing it can only
    // be protected from outside, by the caller declining to send.
    .with_max_input_chars(
        NonZeroU32::try_from(u32::try_from(proxima::llm::MIN_EMBED_INPUT_CAP_CHARS).unwrap())
            .expect("the floor is positive"),
    );
    assert_eq!(caps.dim as usize, proxima::llm::EMBEDDING_DIM);
    assert!(
        caps.max_input_chars.is_some(),
        "the cap survives the builder"
    );

    // Loopback plaintext is accepted; the point is that the signature is
    // writable at all.
    let client = proxima::OpenAiCompatEmbeddingClient::new(
        "some-local-model",
        caps,
        proxima::OpenAiCompatConfig::new("http://localhost:11434/v1", None),
    );
    assert!(client.is_ok(), "a loopback OpenAI-compatible client builds");
}

#[test]
fn host_api_can_build_a_search_read_request() {
    // `Engine::search` was already public, but every type in its signature
    // was off the facade — so a flavor could write a corpus and had no
    // sanctioned way to query it. Constructing the request rather than
    // merely naming the types means a field added or retyped upstream
    // breaks here instead of at some flavor's next pin bump.
    let owner: proxima::Owner = proxima::company_owner(uuid::Uuid::nil());
    let request = proxima::SearchReadRequest {
        search: proxima::MemorySearchRequest {
            owner,
            read_owners: vec![owner],
            query: "Schnittzeichnung Getriebe".to_owned(),
            mode: proxima::SearchMode::Hybrid,
            supersession: proxima::SupersessionStatus::HeadsOnly,
            limit: 5,
            kind: None,
            schema_id: None,
            // The only predicate that narrows a search to part of a corpus.
            tags: vec!["book:0198f0d2".to_owned()],
            tag_match: proxima::TagMatch::Any,
            since: None,
            until: None,
            order: proxima::SearchOrder::Relevance,
            min_score: None,
            semantic_weight: Some(proxima::DEFAULT_HYBRID_SEMANTIC_WEIGHT),
            after: None,
            query_embedding: None,
            embedding_model_id: None,
        },
        include_body: false,
        include_neighbor_edges: false,
    };
    assert!(request.search.limit <= proxima::MAX_SEARCH_PAGE_LIMIT);

    // The response types must be nameable too — a caller has to bind what
    // `search` returns and read a hit out of it.
    let _: Option<(
        &proxima::SearchReadResponse,
        &proxima::MemorySearchResult,
        &proxima::MemorySearchPage,
        &proxima::SearchCursor,
    )> = None;
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

/// A stateful Fact declared through the SDK alone: a head per natural key,
/// plus the discriminator that tells storage which observation means
/// "gone". Both halves are needed — `natural_key_columns` without
/// `tombstone` leaves a deleted entity as a live head forever, because
/// `PresentOnly` has nothing to filter on.
#[derive(serde::Serialize, serde::Deserialize)]
struct TierStatefulFact {
    slot: String,
    state: String,
}

impl proxima::flavor::FactPayload for TierStatefulFact {
    const SCHEMA_ID: &'static str = "proxima-tier/stateful-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn receipt_key(&self) -> Vec<u8> {
        let mut key =
            proxima::flavor::PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
        key.field_str("slot", &self.slot);
        key.field_str("state", &self.state);
        key.finish()
    }

    fn render(&self) -> String {
        String::new()
    }

    fn sidecar_table() -> Option<&'static str> {
        Some("proxima_tier.stateful_v1")
    }

    fn natural_key_columns() -> &'static [&'static str] {
        &["slot"]
    }

    fn tombstone() -> Option<proxima::flavor::FactTombstone> {
        Some(proxima::flavor::FactTombstone {
            column: "state",
            value: "Tombstone",
        })
    }
}

#[test]
fn flavor_sdk_exposes_the_stateful_fact_tombstone() {
    use proxima::flavor::{FactPayload, FactTombstone};

    // Naming the override's return type is the load-bearing half: without
    // `FactTombstone` on the facade the signature above is unspellable, and
    // an out-of-tree flavor cannot declare a stateful Fact schema at all.
    let tombstone: Option<FactTombstone> = <TierStatefulFact as FactPayload>::tombstone();
    let tombstone = tombstone.expect("the schema declares a tombstone discriminator");
    assert_eq!(tombstone.column, "state");
    assert_eq!(tombstone.value, "Tombstone");

    // The other half of the stateful contract travels with it.
    assert_eq!(
        <TierStatefulFact as FactPayload>::natural_key_columns(),
        &["slot"]
    );
}

#[test]
fn flavor_sdk_exposes_the_cited_blob_lane() {
    // `FlavorWorkerContext::blobs` is a public field, so its type and the
    // trait behind it must be nameable from `proxima::flavor` alone — a
    // flavor depending only on `proxima` cannot reach into
    // `proxima_core::storage_ports`.
    use proxima::flavor::{
        CitedBlobPort, CitedBlobReadUrl, CitedBlobService, CitedBlobStaged, CitedBlobUploadAborted,
        CitedBlobUploadCompleted, CitedBlobUploadHeader, CitedBlobUploadPrepared,
    };
    fn _needs_port<T: CitedBlobPort>() {}
    let _: Option<(
        &CitedBlobService,
        &CitedBlobReadUrl,
        &CitedBlobUploadPrepared,
        // `stage_upload` returns this, so a flavor implementing the port
        // must be able to name it.
        &CitedBlobStaged,
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
fn host_api_can_configure_the_blob_lane_without_the_environment() {
    // `Proxima::s3` is a `pub` method and `BuiltProxima::blobs` is a `pub`
    // field of type `Option<CitedBlobStore>`, so both types were already
    // part of the public surface — they just could not be NAMED from
    // `proxima`. A host could reach them by inference and could not write
    // either in a signature, hold one in a struct, or configure S3 at all
    // except through `S3RuntimeConfig::from_env`, which makes process
    // environment a hard requirement of a library API.
    // Nameable in a signature, which is what "the field can be held" means.
    fn _holds_the_lane(_lane: Option<proxima::CitedBlobStore>) {}

    let config = proxima::S3RuntimeConfig {
        bucket: "some-bucket".to_owned(),
        region: "us-east-1".to_owned(),
        endpoint_url: Some("http://localhost:9000".to_owned()),
        force_path_style: true,
        upload_ttl_seconds: 900,
        read_ttl_seconds: 300,
        max_blob_bytes: None,
    };
    assert_eq!(config.bucket, "some-bucket");

    // The builder method that takes it is writable, which is the point.
    std::hint::black_box(
        proxima::Proxima::<TierExtensionApp>::s3
            as fn(
                proxima::Proxima<TierExtensionApp>,
                proxima::S3RuntimeConfig,
            ) -> proxima::Proxima<TierExtensionApp>,
    );
}

#[test]
fn host_api_can_name_the_owner_ref_discriminant() {
    // `OwnerRef::columns()` is public and returns `(OwnerRefKind, Option<Uuid>)`.
    // Every flavor with its own tables calls it to bind owner columns, and
    // could only ever pass the result straight into a query — the moment one
    // wants to store it, return it, or match on it, the type has no name.
    let (kind, id) = proxima::OwnerRef::World.columns();
    assert_eq!(kind, proxima::OwnerRefKind::World);
    assert!(id.is_none());

    let user = proxima::UserId::new(uuid::Uuid::nil());
    let (kind, id) = proxima::OwnerRef::Personal(user).columns();
    assert_eq!(kind, proxima::OwnerRefKind::Personal);
    assert_eq!(id, Some(uuid::Uuid::nil()));
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
