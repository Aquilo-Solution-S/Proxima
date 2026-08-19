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
    assert_send_sync::<proxima::HostAllowlist>();
    assert_send_sync::<proxima::Engine>();
    assert_send_sync::<proxima::DelegationId>();
    assert_send_sync::<proxima::DelegatedCommand>();
    assert_send_sync::<proxima::DelegationIssued>();
    assert_send_sync::<proxima::DelegatedPhase>();
    assert_send_sync::<proxima::DelegatedAuthorityService>();
    let owner: proxima::Owner = proxima::company_owner(uuid::Uuid::nil());
    let _authz: proxima::AuthzContext = proxima::AuthzContext::denied_for_owner(&owner);
    let _narrowed = proxima::AuthzContext::denied_for_owner(&owner).narrowed_to_owner(owner);
    let _cursor: proxima::Cursor = proxima::Cursor::empty();
    let _host_allowlist = proxima::HostAllowlist::new(["proxima.example.com"]);
    let _cancel = proxima::CancellationToken::new();
    std::hint::black_box(proxima::load_source_cursor);
    std::hint::black_box(proxima::store_source_cursor);
    let _outcome = proxima::ComplianceEraseOutcome::Refused {
        operation_id: uuid::Uuid::nil(),
        reason: proxima::ComplianceEraseRefusal::WorldOwner,
    };
}

#[test]
fn delegated_worker_surface_is_available_from_both_supported_facades() {
    fn needs_authority<T: proxima::EngineAuthority + ?Sized>() {}
    fn needs_flavor_authority<T: proxima::flavor::EngineAuthority + ?Sized>() {}

    needs_authority::<proxima::AuthzContext>();
    needs_authority::<proxima::DelegatedPhase>();
    needs_flavor_authority::<proxima::flavor::DelegatedPhase>();

    let _: Option<(
        proxima::DelegationId,
        proxima::DelegatedCommand,
        proxima::DelegationIssued,
        proxima::DelegationRevocation,
        proxima::DelegatedAuthorityError,
    )> = None;
    let _: Option<(
        proxima::flavor::DelegationId,
        proxima::flavor::DelegatedCommand,
        proxima::flavor::DelegationIssued,
        proxima::flavor::DelegationRevocation,
        proxima::flavor::DelegatedAuthorityError,
    )> = None;
}

#[test]
fn flavor_api_reuses_the_shared_endpoint_transport_policy() {
    use proxima::flavor::{
        EndpointUrlError, EndpointUrlPolicy, is_loopback_endpoint, is_loopback_host,
        validate_endpoint_url,
    };

    validate_endpoint_url(
        "http://[::1]:11434/v1",
        EndpointUrlPolicy::AllowLoopbackHttp,
    )
    .expect("loopback plaintext is the one supported exception");
    assert!(is_loopback_host("LOCALHOST"));
    assert!(is_loopback_endpoint("http://127.0.0.1:9000"));

    for deceptive in [
        "http://localhost.evil.example/v1",
        "http://localhost@evil.example/v1",
    ] {
        assert_eq!(
            validate_endpoint_url(deceptive, EndpointUrlPolicy::AllowLoopbackHttp),
            Err(EndpointUrlError::InsecureTransport),
            "deceptive remote endpoint must not inherit loopback plaintext policy: {deceptive}",
        );
    }
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
fn host_api_can_build_an_openai_compatible_embedding_client() {
    // `OpenAiCompatEmbeddingClient` was already exported, but its `new`
    // takes an `EmbedCaps` the facade did not name, leaving the generic
    // constructor out of reach for a host depending on `proxima` alone.
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

/// A stateful Fact declared through the SDK alone: one hot head per natural
/// key, plus catalog metadata identifying which payload value means "gone".
/// Query still returns that deletion-observation head.
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
    // an out-of-tree flavor cannot expose its deletion discriminator.
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
    // A worker resolves this port from `FlavorServices`, so both the handle
    // and the trait behind it must be nameable from `proxima::flavor` alone.
    use proxima::flavor::{
        CitedBlobHeld, CitedBlobIntegrityMismatch, CitedBlobOwnerMissingObject,
        CitedBlobOwnerReconcileOutcome, CitedBlobOwnerReconcilePort,
        CitedBlobOwnerReconcileService, CitedBlobPort, CitedBlobReadError, CitedBlobReadPort,
        CitedBlobReadService, CitedBlobReadUrl, CitedBlobService, CitedBlobStaged,
        CitedBlobUploadAborted, CitedBlobUploadCompleted, CitedBlobUploadHeader,
        CitedBlobUploadPrepared, MAX_HELD_BLOB_DIGESTS, UploadedBlobPayload, VerifiedCitedBlob,
    };
    fn _needs_port<T: CitedBlobPort>() {}
    fn _needs_verified_read_port<T: CitedBlobReadPort>() {}
    fn _needs_owner_reconcile_port<T: CitedBlobOwnerReconcilePort>() {}
    let _: Option<(
        &CitedBlobService,
        &CitedBlobOwnerReconcileService,
        &CitedBlobReadUrl,
        &CitedBlobUploadPrepared,
        &CitedBlobUploadCompleted,
        &CitedBlobUploadAborted,
        &CitedBlobUploadHeader,
    )> = None;
    let _: Option<&CitedBlobReadService> = None;

    let owner_report = CitedBlobOwnerReconcileOutcome {
        rows_scanned: 1,
        objects_scanned: 0,
        missing_objects: 1,
        missing_sample: vec![CitedBlobOwnerMissingObject {
            cited_object_id: uuid::Uuid::nil(),
            byte_len: 1,
            filename: "handbuch.pdf".to_owned(),
        }],
        orphan_objects: 0,
        foreign_locators: 0,
    };
    assert!(!owner_report.is_intact());

    let verified = VerifiedCitedBlob {
        cited_object_id: uuid::Uuid::nil(),
        content_hash: [1; 32],
        sha256: [2; 32],
        byte_len: 1,
        mime: "application/octet-stream".to_owned(),
        filename: "blob.bin".to_owned(),
        bytes: vec![7],
    };
    assert_eq!(verified.bytes, [7]);
    let mismatch = CitedBlobReadError::IntegrityMismatch(CitedBlobIntegrityMismatch::Sha256);
    assert!(matches!(
        mismatch,
        CitedBlobReadError::IntegrityMismatch(CitedBlobIntegrityMismatch::Sha256)
    ));

    // CONSTRUCTED, NOT NAMED. `stage_upload` must RETURN this, and the
    // naming form above passed for a release while the port was
    // unimplementable out-of-tree: `payload` is an `UploadedBlobPayload`,
    // which was not on the facade, so no `use` could complete this
    // literal. A nameability assertion cannot see that, because the
    // unreachable type is a field rather than the type under test.
    let staged = CitedBlobStaged {
        payload: UploadedBlobPayload {
            content_hash: [0u8; 32],
            bucket: "bucket".to_owned(),
            object_key: "objects/aa/bb".to_owned(),
            sha256: [0u8; 32],
            byte_len: 1,
            mime: "application/pdf".to_owned(),
            filename: "handbuch.pdf".to_owned(),
            etag: None,
            uploaded_at: time::OffsetDateTime::UNIX_EPOCH,
        },
        already_completed: None,
    };
    assert!(staged.already_completed.is_none());

    // CONSTRUCTED FOR THE SAME REASON. `find_held_blobs` is a REQUIRED
    // method, so an out-of-tree fake must be able to build its outcome, not
    // merely name it — and a flavor that only ever received these would
    // never notice an unreachable field type. The bound rides along because
    // a caller batching digests has to read it rather than copy it.
    let held = CitedBlobHeld {
        content_hash: [0u8; 32],
        cited_object_id: uuid::Uuid::nil(),
        byte_len: 1,
        mime: "image/jpeg".to_owned(),
        filename: "page-00001.jpg".to_owned(),
    };
    assert_eq!(held.byte_len, 1);
    // USED, not asserted against: its value is a constant, so comparing it
    // proves nothing. What needs proving is that a flavor can reach it to
    // size its own batches — which is exactly this call.
    let batch: Vec<[u8; 32]> = Vec::with_capacity(MAX_HELD_BLOB_DIGESTS);
    assert!(batch.is_empty());
}

#[test]
fn host_api_exposes_role_for_authenticated_adapters() {
    // A host authenticator or trusted adapter can construct a server-resolved
    // group context. Without `Role` on the host facade the constructor's
    // parameter type is unnameable. Delegated workers use `DelegatedPhase`
    // instead of constructing this context themselves.
    let owner: proxima::Owner = proxima::company_owner(uuid::Uuid::nil());
    let _authz: proxima::AuthzContext = proxima::AuthzContext::for_subject_with_role(
        proxima::UserId::new(uuid::Uuid::nil()),
        [(owner, proxima::Role::admin())],
        proxima::AuthPath::HostBearer,
    );

    // `Role::new` / `Role::may_write` / `OwnerRoles::for_subject` name these.
    // Hosts that only imported `Role` had to take `proxima-core` to spell them.
    let ceiling = proxima::AccessCeiling::Fact;
    let role = proxima::Role::new(ceiling, ceiling, false).expect("write <= read");
    assert!(role.may_write(proxima::AccessKind::Fact));
    let user = proxima::UserId::new(uuid::Uuid::nil());
    let group = proxima::OwnerRef::Group(proxima::GroupId::new(uuid::Uuid::nil()));
    let _roles = proxima::OwnerRoles::for_subject(user, [(group, role)]).expect("group roles");
    let _: Option<proxima::AccessError> = None;
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
        AbstractionPayload, AuthorDerivedRequestInput, EdgeEndpoint, EntityKind, InputContractId,
        MemoryId, MemoryOperatorKind, OperatorId, SchemaVersion, SidecarPayload,
    };

    let owner: proxima::Owner = proxima::company_owner(uuid::Uuid::nil());
    let derived = MemoryId::new(uuid::Uuid::nil());
    let source_fact = MemoryId::new(uuid::Uuid::nil());

    // What the write was made from, as endpoints. There is no kind here to
    // pass and no relation to resolve: the entries become `origin` rows
    // because of which field they arrived in (docs/16 §The Model).
    let derived_from = [EdgeEndpoint::memory(EntityKind::Fact, source_fact)];

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
        model_id: "tier-test",
        sidecar_payload: SidecarPayload::abstraction(TierAbstraction {
            note: "sidecar".to_owned(),
        }),
        derived_from: &derived_from,
        extra_refs: &[],
        supersedes: None,
        lexical_language: None,
    };

    // The outcome type must be nameable too — a caller has to bind what
    // `author_derived_authorized` returns.
    let _: Option<&proxima::flavor::AuthorDerivedAuthorizedOutcome> = None;
}

/// A payload that points at another node, built through the facade alone.
///
/// This is the whole of how an out-of-tree flavor creates a connection: it
/// declares reference fields, ingest reads them, and the index rows follow.
/// If [`PayloadReference`] or its binding vocabulary were missing from the
/// facade, the override could not be written at all and a flavor's schemas
/// could only ever be islands.
#[test]
fn flavor_sdk_exposes_the_payload_reference_lane() {
    use proxima::flavor::{EdgeKind, EntityKind, MemoryId, PayloadReference, ReferenceBinding};

    struct TierReferrer {
        parent: MemoryId,
    }

    impl TierReferrer {
        fn references(&self) -> Vec<PayloadReference> {
            vec![PayloadReference::memory(
                "parent",
                EntityKind::Fact,
                self.parent,
            )]
        }
    }

    let referrer = TierReferrer {
        parent: MemoryId::new(uuid::Uuid::nil()),
    };
    let references = referrer.references();
    assert_eq!(references.len(), 1);
    assert_eq!(references[0].binding, ReferenceBinding::Pin);
    for reference in &references {
        reference.validate().expect("binding matches address form");
    }

    // A kind is read, never written: it is what an edge IS, decided by the
    // write that produced the row.
    assert_eq!(EdgeKind::Reference.as_str(), "reference");
    assert_eq!(EdgeKind::Origin.as_str(), "origin");
}

/// A flavor-owned service, standing in for the store a real flavor hands
/// its tools (`proxima-code` puts a `CodeFlavorStore` here). Core cannot
/// name this type, which is the entire point of the service map.
struct TierFlavorStore {
    marker: &'static str,
}

/// The override a flavor host must be able to *write*. This is the load-
/// bearing half: `services` returns `FlavorServices`, so
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

    fn services(
        ctx: &proxima::AppContext,
    ) -> Result<proxima::flavor::FlavorServices, proxima::flavor::FlavorServiceError> {
        // A real host composes its store from `ctx.clone_pool_for_host()`.
        // This test only pins that the override is writable; the bridge
        // itself is named in `host_extra_table_bridge_is_on_app_context`.
        let _ = ctx;
        let mut services = proxima::flavor::FlavorServices::default();
        services.try_insert(TierFlavorStore { marker: "store" })?;
        Ok(services)
    }
}

#[test]
fn flavor_sdk_exposes_the_flavor_service_seam() {
    use proxima::flavor::{FlavorServiceError, FlavorServices};

    // Both halves must be reachable: the host inserts a service core
    // cannot name, and the tool resolves it back by type.
    let mut services = FlavorServices::default();
    services
        .try_insert(TierFlavorStore { marker: "store" })
        .unwrap();
    let resolved = services
        .get::<TierFlavorStore>()
        .expect("a tool must resolve the service its host inserted");
    assert_eq!(resolved.marker, "store");

    // The one-shot constructor is the common case for a single service.
    let single = FlavorServices::with(TierFlavorStore { marker: "single" });
    assert_eq!(
        single
            .get::<TierFlavorStore>()
            .expect("with() inserts the value")
            .marker,
        "single"
    );

    // An absent service resolves to None rather than panicking.
    assert!(single.get::<u64>().is_none());

    // And the override itself is nameable; calling it needs an AppContext
    // only a booted runtime can make, so this pins the signature.
    std::hint::black_box(
        <TierExtensionApp as proxima::FlavorApp>::services
            as fn(&proxima::AppContext) -> Result<FlavorServices, FlavorServiceError>,
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
fn host_api_can_configure_the_pg_pool_without_process_environment() {
    let config = proxima::PgPoolConfig {
        max_connections: 4,
        statement_timeout: std::time::Duration::from_secs(30),
        acquire_timeout: std::time::Duration::from_secs(2),
        idle_timeout: std::time::Duration::from_mins(1),
        max_lifetime: std::time::Duration::from_mins(2),
    };
    assert_eq!(config.max_connections, 4);

    std::hint::black_box(
        proxima::RuntimeBuilder::pg_pool_config
            as fn(proxima::RuntimeBuilder, proxima::PgPoolConfig) -> proxima::RuntimeBuilder,
    );
    std::hint::black_box(
        proxima::Proxima::<TierExtensionApp>::pg_pool_config
            as fn(
                proxima::Proxima<TierExtensionApp>,
                proxima::PgPoolConfig,
            ) -> proxima::Proxima<TierExtensionApp>,
    );
}

#[test]
fn host_api_names_global_and_owner_reconcile_outcomes() {
    // `CitedBlobStore::reconcile_all` is a `pub async fn` reachable
    // through `AppContext::blobs` / `BuiltProxima::blobs` /
    // `RunningProxima::blobs`. Calling it also requires the booted runtime's
    // same-boot, non-Clone `SystemAuthority`. The owner service is a distinct,
    // authorization-carrying lane with a locator-free result.
    use proxima::{
        CitedBlobMissingObject, CitedBlobOwnerMissingObject, CitedBlobOwnerReconcileOutcome,
        CitedBlobOwnerReconcileService, CitedBlobReadError, CitedBlobReadService,
        CitedBlobReconcileOutcome, MAX_RECONCILE_SAMPLE, VerifiedCitedBlob,
    };

    // Nameable in a signature, which is what "the return type can be held"
    // means.
    fn _returns_the_outcome() -> proxima::CitedBlobReconcileOutcome {
        CitedBlobReconcileOutcome::default()
    }

    // CONSTRUCTED, NOT NAMED: a nameability check alone would not notice an
    // unreachable field type on `missing_sample`'s element.
    let missing = CitedBlobMissingObject {
        cited_object_id: uuid::Uuid::nil(),
        object_key: "objects/aa/bb".to_owned(),
        byte_len: 1,
        filename: "handbuch.pdf".to_owned(),
    };

    let outcome = CitedBlobReconcileOutcome {
        rows_scanned: 1,
        objects_scanned: 0,
        missing_objects: 1,
        missing_sample: vec![missing],
        orphan_objects: 0,
        orphan_sample: Vec::new(),
        foreign_locators: 0,
        foreign_sample: Vec::new(),
    };
    assert!(!outcome.is_intact());
    assert_eq!(outcome.missing_sample[0].filename, "handbuch.pdf");

    // USED, not asserted against: its value is a constant, so comparing it
    // proves nothing. What needs proving is that a host can reach it to
    // size its own buffers against the sample bound.
    let buffer: Vec<CitedBlobMissingObject> = Vec::with_capacity(MAX_RECONCILE_SAMPLE);
    assert!(buffer.is_empty());

    let owner_outcome = CitedBlobOwnerReconcileOutcome {
        rows_scanned: 1,
        objects_scanned: 0,
        missing_objects: 1,
        missing_sample: vec![CitedBlobOwnerMissingObject {
            cited_object_id: uuid::Uuid::nil(),
            byte_len: 1,
            filename: "handbuch.pdf".to_owned(),
        }],
        orphan_objects: 0,
        foreign_locators: 0,
    };
    let _: Option<CitedBlobOwnerReconcileService> = None;
    let _: Option<(CitedBlobReadService, CitedBlobReadError, VerifiedCitedBlob)> = None;
    assert!(!owner_outcome.is_intact());
    assert_eq!(
        owner_outcome.missing_sample[0].cited_object_id,
        uuid::Uuid::nil()
    );
}

#[test]
fn host_names_mcp_catalog_descriptor_types() {
    // `FlavorRegistryFrozen::list_mcp_tools` already returns this slice.
    // The element type and origin enum must be nameable on the Host API.
    use proxima::host::{FlavorRegistryFrozen, McpToolDescriptor, McpToolOrigin};

    fn bind(frozen: &FlavorRegistryFrozen) -> &[McpToolDescriptor] {
        frozen.list_mcp_tools()
    }

    let frozen = proxima::flavor::FlavorRegistry::new()
        .try_freeze()
        .expect("empty registry freezes");
    let named = bind(&frozen)
        .iter()
        .filter(|tool| {
            matches!(
                tool.origin,
                McpToolOrigin::Substrate | McpToolOrigin::Flavor(_)
            )
        })
        .count();
    assert!(
        named >= 1,
        "substrate tools register on FlavorRegistry::new"
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
    let authorized_read = include_str!("../src/flavor/authorized_read.rs");

    // `PgPoolConfig` is pure policy, not the raw SQLx handle this guard bans.
    assert!(!host_exports.replace("PgPoolConfig", "").contains("PgPool"));
    assert!(!host_exports.contains("PgStorage"));
    assert!(!host_exports.contains("StorageHandle"));
    assert!(!flavor_exports.contains("PgPool"));
    assert!(!flavor_exports.contains("PgStorage"));
    assert!(!flavor_exports.contains("StorageHandle"));
    assert!(
        !authorized_read.contains("use sqlx::PgPool") && !authorized_read.contains("&PgPool"),
        "code-series pool helpers must not live on the Flavor SDK"
    );
}

#[test]
fn host_extra_table_bridge_is_on_app_context() {
    // The one sanctioned PgPool leak is AppContext::clone_pool_for_host
    // (docs/08). Flavor SDK still must not name the type. app.rs is
    // scanned so this cannot hide in a submodule the host.rs scan misses.
    let flavor_exports = include_str!("../src/flavor.rs");
    let authorized_read = include_str!("../src/flavor/authorized_read.rs");
    let app = include_str!("../src/app.rs");

    assert!(!flavor_exports.contains("PgPool"));
    assert!(
        !authorized_read.contains("use sqlx::PgPool") && !authorized_read.contains("&PgPool"),
        "code-series pool helpers must not live on the Flavor SDK"
    );
    assert!(
        app.contains("pub fn clone_pool_for_host"),
        "host extra-table bridge must stay on AppContext"
    );
    assert!(
        !app.contains("pub pool"),
        "the pool field stays crate-private"
    );
    let _: fn(&proxima::AppContext) -> sqlx::PgPool = proxima::AppContext::clone_pool_for_host;
}

/// Naming [`proxima::flavor::AuthorizationHook`] is not enough: `veto`
/// takes [`proxima::flavor::AuthzInput`] and returns [`proxima::flavor::AuthzVeto`].
#[derive(Debug)]
struct TierAuthzHook;

impl proxima::flavor::AuthorizationHook for TierAuthzHook {
    fn veto(
        &self,
        input: &proxima::flavor::AuthzInput<'_>,
    ) -> Result<(), proxima::flavor::AuthzVeto> {
        match &input.operation {
            proxima::flavor::AuthzOperation::Relation { .. }
            | proxima::flavor::AuthzOperation::Membership {
                change: proxima::flavor::MembershipChange::Add,
                ..
            }
            | proxima::flavor::AuthzOperation::EntityShare { .. } => Ok(()),
            proxima::flavor::AuthzOperation::Membership {
                change: proxima::flavor::MembershipChange::Remove,
                ..
            } => Err(proxima::flavor::AuthzVeto("denied".into())),
        }
    }

    fn observe(
        &self,
        _input: &proxima::flavor::AuthzInput<'_>,
        outcome: proxima::flavor::AuthzOutcome,
    ) {
        let _ = matches!(outcome, proxima::flavor::AuthzOutcome::Allowed);
    }
}

#[derive(Debug)]
struct TierOwnerResolver;

impl proxima::flavor::OwnerResolver for TierOwnerResolver {
    fn resolve(
        &self,
        _authz: &proxima::AuthzContext,
        requested: &proxima::Owner,
    ) -> Result<proxima::Owner, proxima::ProtocolError> {
        Ok(*requested)
    }
}

#[test]
fn flavor_sdk_names_query_and_ingest_types() {
    use proxima::flavor::{AuthorizationHook, OwnerResolver};

    fn assert_hook<T: AuthorizationHook + ?Sized>() {}
    fn assert_resolver<T: OwnerResolver + ?Sized>() {}
    assert_hook::<dyn AuthorizationHook>();
    assert_resolver::<dyn OwnerResolver>();

    let owner = proxima::Owner::World;
    let authz = proxima::AuthzContext::single_owner(&owner, proxima::AuthPath::System);
    let input = proxima::flavor::AuthzInput {
        authz: &authz,
        requested: &owner,
        resolved: &owner,
        relation: proxima::Relation::Viewer,
        operation: proxima::flavor::AuthzOperation::Relation {
            relation: proxima::Relation::Viewer,
        },
    };
    TierAuthzHook.veto(&input).expect("relation allow");
    TierAuthzHook.observe(&input, proxima::flavor::AuthzOutcome::Allowed);
    let share = proxima::flavor::AuthzInput {
        authz: &authz,
        requested: &owner,
        resolved: &owner,
        relation: proxima::Relation::Admin,
        operation: proxima::flavor::AuthzOperation::EntityShare {
            entity: proxima::flavor::EntityId::Memory(proxima::MemoryId::new(uuid::Uuid::nil())),
            owner,
        },
    };
    TierAuthzHook.veto(&share).expect("share allow");
    assert_eq!(
        TierOwnerResolver.resolve(&authz, &owner).expect("resolve"),
        owner
    );

    let mut registry = proxima::flavor::FlavorRegistry::new();
    registry.add_authorization_hook(std::sync::Arc::new(TierAuthzHook));
    registry
        .try_set_owner_resolver(std::sync::Arc::new(TierOwnerResolver))
        .expect("one resolver");

    let _ = proxima::flavor::SidecarAtom::I32(0);
    let _ = proxima::flavor::CitationSpec::v1("core/upload-v1", [0; 32], "core/upload-whole-v1");
    let fact = TierStatefulFact {
        slot: "a".into(),
        state: "Present".into(),
    };
    let _cite = proxima::flavor::TypedFactIngest::new("test/src", &fact).citation(
        proxima::flavor::CitationSpec::v1("core/upload-v1", [0; 32], "core/upload-whole-v1"),
    );
    let _: Option<proxima::flavor::UnitOfWork<'_>> = None;
    assert!(proxima::flavor::hybrid_degraded_to_lexical(
        proxima::flavor::SearchMode::Hybrid,
        false,
        false,
    ));
    assert!(!proxima::flavor::hybrid_degraded_to_lexical(
        proxima::flavor::SearchMode::Lexical,
        false,
        false,
    ));

    let owner = proxima::OwnerRef::World;
    let _: proxima::flavor::QueryRequest = proxima::flavor::QueryRequest::for_owner(owner);
    let _: Option<(
        proxima::flavor::QueryResponse,
        proxima::flavor::GoalRow,
        proxima::flavor::FactIngestOutcome,
        proxima::flavor::FactWriteCommand,
        proxima::flavor::AuthorizedFactWithCitation,
        proxima::flavor::AuthorizedFactWithCitationRef,
    )> = None;
    let _cited = proxima::flavor::InlineCitedObjectDraft {
        schema_id: proxima::flavor::SchemaId::new("core/upload-v1".into()),
        schema_version: proxima::flavor::SchemaVersion::new(1),
        payload_bytes: Vec::new(),
    };
    let _mapping = proxima::flavor::InlineCitationMappingDraft {
        schema_id: proxima::flavor::SchemaId::new("core/upload-whole-v1".into()),
        schema_version: proxima::flavor::SchemaVersion::new(1),
        payload_bytes: Vec::new(),
    };
}

#[cfg(feature = "auth-oidc")]
#[test]
fn auth_module_names_oidc_primitives() {
    let _: Option<(
        proxima::auth::OidcAuthConfig,
        proxima::auth::OidcTokenValidator,
        proxima::auth::ValidatedOidcClaims,
        proxima::auth::HttpJwksResolver,
        proxima::auth::AccessError,
        proxima::auth::OwnerRoles,
    )> = None;
    let _: fn(
        &proxima::BuiltProxima,
        proxima::flavor::FlavorServices,
    ) -> Result<proxima::CoreMcpTools, proxima::flavor::FlavorServiceError> =
        proxima::BuiltProxima::core_mcp_tools_with_request_services;
}
