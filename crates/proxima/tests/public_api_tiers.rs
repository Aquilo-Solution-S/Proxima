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
    // A worker resolves this port from `FlavorServices`, so both the handle
    // and the trait behind it must be nameable from `proxima::flavor` alone.
    use proxima::flavor::{
        CitedBlobHeld, CitedBlobPort, CitedBlobReadUrl, CitedBlobService, CitedBlobStaged,
        CitedBlobUploadAborted, CitedBlobUploadCompleted, CitedBlobUploadHeader,
        CitedBlobUploadPrepared, MAX_HELD_BLOB_DIGESTS, UploadedBlobPayload,
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
        source_batch_id: None,
        model_id: "tier-test",
        prompt_version: "1",
        sidecar_payload: SidecarPayload::abstraction(TierAbstraction {
            note: "sidecar".to_owned(),
        }),
        authoring_perspective_id: None,
        derived_from: &derived_from,
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
    use proxima::flavor::{
        EdgeKind, EntityKind, FactEntityId, MemoryId, PayloadReference, ReferenceBinding,
    };

    struct TierReferrer {
        parent: MemoryId,
        observed_entity: FactEntityId,
    }

    impl TierReferrer {
        fn references(&self) -> Vec<PayloadReference> {
            vec![
                PayloadReference::memory("parent", EntityKind::Fact, self.parent),
                PayloadReference::fact_entity_head("observed_entity", self.observed_entity),
            ]
        }
    }

    let referrer = TierReferrer {
        parent: MemoryId::new(uuid::Uuid::nil()),
        observed_entity: FactEntityId::new(uuid::Uuid::nil()),
    };
    let references = referrer.references();
    assert_eq!(references.len(), 2);
    assert_eq!(references[0].binding, ReferenceBinding::Pin);
    assert_eq!(references[1].binding, ReferenceBinding::FollowHead);
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
        // The pool stays off the supported tier deliberately (see
        // `raw_storage_surfaces_are_not_supported_tier_exports`), so this
        // test only pins that the override is writable at all.
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
fn host_api_names_the_reconcile_outcome_it_returns() {
    // `CitedBlobStore::reconcile_cited_blobs` is a `pub async fn` reachable
    // through `AppContext::blobs` / `BuiltProxima::blobs` /
    // `RunningProxima::blobs` (all `Option<CitedBlobStore>`, pinned
    // nameable above), and it returns `CitedBlobReconcileOutcome`. Before
    // this export a host could call the method and bind its result only by
    // inference — no signature could hold it, no struct field could carry
    // it forward, and nothing beyond `is_intact()` could be matched on.
    use proxima::{CitedBlobMissingObject, CitedBlobReconcileOutcome, MAX_RECONCILE_SAMPLE};

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
