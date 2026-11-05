use std::num::NonZeroU32;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct SdkGoalPayload {
    key: String,
}

impl proxiima::spiice::GoalPayload for SdkGoalPayload {
    const SCHEMA_ID: &'static str = "sdk_test/goal-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn goal_key(&self) -> Vec<u8> {
        let mut key =
            proxiima::spiice::PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
        key.field_str("key", &self.key);
        key.finish()
    }
}

impl proxiima::spiice::PayloadProjection for SdkGoalPayload {
    const KEY_FIELDS: &'static [proxiima::spiice::KeyField<Self>] = &[];
    fn display(&self) -> proxiima::spiice::PayloadDisplay {
        proxiima::spiice::PayloadDisplay::default()
    }
    fn durable(&self) -> Result<proxiima::spiice::DurablePayload, String> {
        let mut builder = proxiima::spiice::DurablePayloadBuilder::new();
        builder.field("key", &self.key)?;
        Ok(builder.finish())
    }
}

/// A host that holds its own transaction resolves roles on it through the
/// facade: the connection-taking probes, not a second copy of the queries.
#[test]
fn host_api_names_the_connection_role_probes() {
    async fn probes(
        conn: &mut sqlx::PgConnection,
        subject: proxiima::UserId,
        group: proxiima::GroupId,
    ) -> Result<(proxiima::OwnerRoles, Option<proxiima::Role>), proxiima::AccessError> {
        let roles = proxiima::roles_for_subject(&mut *conn, subject).await?;
        let role = proxiima::group_role(conn, subject, group).await?;
        Ok((roles, role))
    }
    std::hint::black_box(probes);
}

#[test]
fn host_api_imports_from_root() {
    fn assert_send_sync<T: Send + Sync>() {}
    fn needs_host_state_command<T: proxiima::HostStateCommand>() {}
    fn needs_participant<T: proxiima::PgHostStateParticipant + ?Sized>() {}
    assert_send_sync::<proxiima::OwnerEraseCounts>();
    assert_send_sync::<proxiima::OwnerEraseOutcome>();
    assert_send_sync::<proxiima::OwnerEraseRefusal>();
    assert_send_sync::<proxiima::OwnerEraseRequest>();
    assert_send_sync::<proxiima::OwnerEraseTarget>();
    assert_send_sync::<proxiima::CancellationToken>();
    assert_send_sync::<proxiima::FeatureControl>();
    assert_send_sync::<proxiima::FeatureStatus>();
    assert_send_sync::<proxiima::FeatureControlError>();
    assert_send_sync::<proxiima::RuntimeBuilder>();
    assert_send_sync::<proxiima::RuntimeConfig>();
    assert_send_sync::<proxiima::HostAllowlist>();
    assert_send_sync::<proxiima::Engine>();
    assert_send_sync::<proxiima::DelegationId>();
    assert_send_sync::<proxiima::DelegatedCommand>();
    assert_send_sync::<proxiima::DelegationIssued>();
    assert_send_sync::<proxiima::DelegatedPhase>();
    assert_send_sync::<proxiima::DelegatedAuthorityService>();
    let owner: proxiima::OwnerId = proxiima::company_owner(uuid::Uuid::nil());
    let _authz: proxiima::AuthzContext = proxiima::AuthzContext::denied_for_owner(&owner);
    let _narrowed = proxiima::AuthzContext::denied_for_owner(&owner).narrowed_to_owner(owner);
    let _cursor: proxiima::Cursor = proxiima::Cursor::empty();
    let _host_allowlist = proxiima::HostAllowlist::new(["proxiima.example.com"]);
    let _cancel = proxiima::CancellationToken::new();
    let _outcome = proxiima::OwnerEraseOutcome::Refused {
        operation_id: uuid::Uuid::nil(),
        reason: proxiima::OwnerEraseRefusal::OwnerNotAbandoned,
    };
    let _: Option<(
        proxiima::HostStateOutcome<()>,
        proxiima::HostStateReplyKind,
        proxiima::HostStateRequest,
        proxiima::HostStateReply,
    )> = None;
    needs_participant::<dyn proxiima::PgHostStateParticipant>();
    needs_host_state_command::<HostStateTierCommand>();
}

struct HostStateTierCommand;

impl proxiima::HostStateCommand for HostStateTierCommand {
    const PARTICIPANT_ID: proxiima::HostStateParticipantId =
        proxiima::HostStateParticipantId::new("tier");
    const TABLES: &'static [proxiima::StateSurfaceName] =
        &[proxiima::StateSurfaceName::new("tier.table")];
    type Outcome = ();

    fn owner(&self) -> proxiima::OwnerId {
        proxiima::company_owner(uuid::Uuid::nil())
    }
}

/// A host builds palettes from typed keys; the facade names every piece of
/// them, and the keys print the strings the env and docs spell.
#[test]
fn host_api_builds_a_palette_from_typed_scope_keys() {
    use proxiima::host::{ActionName, ResourceKey, ScopeKey, ScopeKeyError, ToolName, ToolScope};

    let tool = ToolName::parse("host_echo").expect("a tool name");
    let goal = ToolName::parse("core_goal").expect("a tool name");
    let set = ActionName::parse("set").expect("an action name");
    let scope = ToolScope::Palette(vec![
        ScopeKey::Tool(tool),
        ScopeKey::action(goal.clone(), set.clone()).expect("a leaf"),
        ScopeKey::Resource(ResourceKey::parse("memory").expect("a resource name")),
    ]);
    assert!(scope.allows_action(&goal, &set));
    assert_eq!(
        ScopeKey::parse("resource:memory")
            .expect("a resource key")
            .to_string(),
        "resource:memory"
    );

    let refusals: [Result<ScopeKey, ScopeKeyError>; 3] = [
        ScopeKey::parse("a:b:c"),
        ScopeKey::parse(""),
        ScopeKey::action(ToolName::parse("resource").expect("a tool name"), set),
    ];
    assert!(refusals.iter().all(Result::is_err));
}

#[test]
fn caller_scope_and_deployment_surface_are_named_through_the_facade() {
    use proxiima::host::{CapabilitySet, RuntimeBuilder, RuntimeConfig, ServedSurface, ToolScope};

    let _: fn(RuntimeBuilder, ServedSurface) -> RuntimeBuilder = RuntimeBuilder::served_surface;
    let _: fn(&RuntimeConfig) -> &ServedSurface = |config| &config.served_surface;
    let capabilities = CapabilitySet {
        tool_scope: ToolScope::Palette(Vec::new()),
        served_surface: ServedSurface::All,
    };
    assert_eq!(capabilities.tool_scope, ToolScope::Palette(Vec::new()));
    assert_eq!(capabilities.served_surface, ServedSurface::All);

    let key = proxiima::spiice::ScopeKey::parse("core_search_memories").expect("a tool key");
    let sdk_capabilities = proxiima::spiice::CapabilitySet {
        tool_scope: proxiima::spiice::ToolScope::Palette(vec![key.clone()]),
        served_surface: proxiima::spiice::ServedSurface::Palette(Vec::new()),
    };
    let read =
        proxiima::spiice::KeyDeclaration::shared(Some(proxiima::spiice::ToolEffect::ReadOnly));
    assert!(sdk_capabilities.admits(&key, read));
    assert!(!sdk_capabilities.admits(
        &proxiima::spiice::ScopeKey::parse("resource:how-to").expect("a resource key"),
        read,
    ));
}

#[test]
fn delegated_worker_surface_is_available_from_both_supported_facades() {
    fn needs_authority<T: proxiima::EngineAuthority + ?Sized>() {}
    fn needs_spiice_authority<T: proxiima::spiice::EngineAuthority + ?Sized>() {}

    needs_authority::<proxiima::AuthzContext>();
    needs_authority::<proxiima::DelegatedPhase>();
    needs_spiice_authority::<proxiima::spiice::DelegatedPhase>();

    let _: Option<(
        proxiima::DelegationId,
        proxiima::DelegatedCommand,
        proxiima::DelegationIssued,
        proxiima::DelegationRevocation,
        proxiima::DelegatedAuthorityError,
    )> = None;
    let _: Option<(
        proxiima::spiice::DelegationId,
        proxiima::spiice::DelegatedCommand,
        proxiima::spiice::DelegationIssued,
        proxiima::spiice::DelegationRevocation,
        proxiima::spiice::DelegatedAuthorityError,
    )> = None;
}

#[test]
fn spiice_api_reuses_the_shared_endpoint_transport_policy() {
    use proxiima::spiice::{EndpointUrlError, EndpointUrlPolicy, validate_endpoint_url};

    validate_endpoint_url(
        "http://[::1]:11434/v1",
        EndpointUrlPolicy::AllowLoopbackHttp,
    )
    .expect("loopback plaintext is the one supported exception");

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
fn host_api_can_construct_every_owner_erase_target() {
    // `OwnerEraseTarget` was already on the facade, but two of its
    // five variants take a `GroupId`/`SourceId` the facade did not name,
    // so a host depending on `proxiima` alone could not build them. An
    // exported enum whose variants are unconstructible is not exported.
    let group_id = proxiima::GroupId::new(uuid::Uuid::nil());
    let source_id = proxiima::SourceId::new("proxiima-tier/scope/0");
    let user_id = proxiima::UserId::new(uuid::Uuid::nil());

    let targets: [proxiima::OwnerEraseTarget; 4] = [
        proxiima::OwnerEraseTarget::GroupOwner { group_id },
        proxiima::OwnerEraseTarget::PersonalOwner {
            user_id,
            drop_event_id: "drop-1".to_owned(),
        },
        proxiima::OwnerEraseTarget::GroupSourceScope {
            group_id,
            source_id: source_id.clone(),
        },
        proxiima::OwnerEraseTarget::PersonalSourceScope {
            user_id,
            source_id,
            drop_event_id: "drop-1".to_owned(),
        },
    ];

    // Naming the count pins the enum's shape: a fifth variant has to be
    // added here, which is the prompt to check it is constructible too.
    assert_eq!(targets.len(), 4);
}

#[test]
fn host_api_can_construct_every_series_selection() {
    // The host schedules retention through `erase_own_series`, so every
    // `SeriesSelection` variant must be buildable from the host facade:
    // three of them name a `SchemaId`, one a `SidecarAtom`.
    let schema = || proxiima::SchemaId::new("proxiima-tier/thing-v1".to_owned());
    let cutoff = time::OffsetDateTime::UNIX_EPOCH;
    let selections: [proxiima::SeriesSelection; 4] = [
        proxiima::SeriesSelection::Ids(vec![proxiima::MemoryId::new(uuid::Uuid::nil())]),
        proxiima::SeriesSelection::AdmittedBefore {
            schema: schema(),
            cutoff,
        },
        proxiima::SeriesSelection::SidecarEquals {
            schema: schema(),
            predicates: vec![("kind".to_owned(), proxiima::SidecarAtom::Bool(true))],
        },
        proxiima::SeriesSelection::DeclaredBefore {
            schema: schema(),
            column: "happened_at".to_owned(),
            cutoff,
        },
    ];
    assert_eq!(selections.len(), 4);
}

#[test]
fn host_api_can_ask_for_and_hold_an_owner_export() {
    // `Engine::export_owner_bundle` is `pub`, takes a
    // `OwnerExportTarget` and returns a `OwnerExportBundle`, and
    // neither was on the facade: a host could call the verb, could not
    // build its argument, and could not name what came back. Erase was
    // reachable and export was not, so the destructive half of an owner's
    // rights shipped without the portable half.
    // Naming the return type is the half that inference cannot supply.
    fn count_rows(bundle: &proxiima::OwnerExportBundle, table: &str) -> usize {
        bundle.table(table).len()
    }
    let _ = count_rows;

    let targets: [proxiima::OwnerExportTarget; 2] = [
        proxiima::OwnerExportTarget::GroupOwner {
            group_id: proxiima::GroupId::new(uuid::Uuid::nil()),
        },
        proxiima::OwnerExportTarget::PersonalOwner {
            user_id: proxiima::UserId::new(uuid::Uuid::nil()),
        },
    ];
    assert_eq!(targets.len(), 2);

    let request = proxiima::OwnerExportRequest {
        target: targets[1].clone(),
    };
    assert!(matches!(
        request.target,
        proxiima::OwnerExportTarget::PersonalOwner { .. }
    ));
}

#[test]
fn host_api_names_the_citation_schema_id() {
    // `CitationSpec::v1` takes `impl Into<String>`, so a missing constant
    // never fails at the call site — it just pushes the caller onto a
    // bare literal that silently stops matching when the constant it
    // duplicates is renamed. The cited-object id is named here so a
    // rename breaks this test instead of a spiice's citations.
    let spec = proxiima::CitationSpec::v1(proxiima::UPLOADED_BLOB_SCHEMA_ID, [0u8; 32]);
    assert_eq!(
        spec.cited_object_schema_id.as_str(),
        proxiima::UPLOADED_BLOB_SCHEMA_ID
    );
}

#[test]
fn host_api_can_build_an_openai_compatible_embedding_client() {
    // `OpenAiCompatEmbeddingClient` was already exported, but its `new`
    // takes an `EmbedCaps` the facade did not name, leaving the generic
    // constructor out of reach for a host depending on `proxiima` alone.
    // Built through the constructor, not a struct literal, and deliberately:
    // a literal names every field, so each new capability axis breaks every
    // out-of-tree host that ever built one. `new` + `with_*` keeps them
    // compiling across a version that adds an axis they do not set.
    let caps = proxiima::EmbedCaps::new(
        u32::try_from(proxiima::llm::EmbeddingDim::D768.width()).expect("the width fits u32"),
        // The reason a local endpoint needs this at all: a model whose
        // native width is no supported `EmbeddingDim` must be asked for a
        // nested prefix, or binding the client refuses it.
        true,
    )
    // The other reason a local endpoint needs the facade to name this: a
    // runner that dies on over-long input rather than refusing it can only
    // be protected from outside, by the caller declining to send.
    .with_max_input_chars(
        NonZeroU32::try_from(u32::try_from(proxiima::llm::MIN_EMBED_INPUT_CAP_CHARS).unwrap())
            .expect("the floor is positive"),
    );
    assert_eq!(caps.dim as usize, proxiima::llm::EmbeddingDim::D768.width());
    assert!(
        caps.max_input_chars.is_some(),
        "the cap survives the builder"
    );

    // Loopback plaintext is accepted; the point is that the signature is
    // writable at all.
    let client = proxiima::OpenAiCompatEmbeddingClient::new(
        "some-local-model",
        caps,
        proxiima::OpenAiCompatConfig::new("http://localhost:11434/v1", None),
    );
    assert!(client.is_ok(), "a loopback OpenAI-compatible client builds");
}

#[test]
fn host_api_can_build_a_search_read_request() {
    // `Engine::search` was already public, but every type in its signature
    // was off the facade — so a spiice could write a corpus and had no
    // sanctioned way to query it. Constructing the request rather than
    // merely naming the types means a field added or retyped upstream
    // breaks here instead of at some spiice's next pin bump.
    let owner: proxiima::OwnerId = proxiima::company_owner(uuid::Uuid::nil());
    let request = proxiima::SearchReadRequest {
        search: proxiima::MemorySearchRequest {
            scope: proxiima::MemorySearchScope::Owner(owner),
            read_owners: vec![owner],
            query: "Schnittzeichnung Getriebe".to_owned(),
            mode: proxiima::SearchMode::Hybrid,
            supersession: proxiima::SupersessionStatus::HeadsOnly,
            limit: 5,
            kind: None,
            schema_id: None,
            // The only predicate that narrows a search to part of a corpus.
            tags: vec!["book:0198f0d2".to_owned()],
            tag_match: proxiima::TagMatch::Any,
            since: None,
            until: None,
            order: proxiima::SearchOrder::Relevance,
            min_score: None,
            semantic_weight: Some(proxiima::DEFAULT_HYBRID_SEMANTIC_WEIGHT),
            after: None,
            semantic: None::<proxiima::llm::SpaceVector>,
        },
        include_body: false,
        include_neighbor_edges: false,
    };
    assert!(request.search.limit <= proxiima::MAX_SEARCH_PAGE_LIMIT);

    // The response types must be nameable too — a caller has to bind what
    // `search` returns and read a hit out of it.
    let _: Option<(
        &proxiima::SearchReadResponse,
        &proxiima::MemorySearchResult,
        &proxiima::MemorySearchPage,
        &proxiima::SearchCursor,
    )> = None;
}

#[test]
fn host_api_can_build_a_batch_memory_read_request() {
    // Binding the response's public field proves the full Engine signature is
    // usable from the facade, without constructing a snapshot and widening
    // this host-tier test into every nested payload type.
    fn memories(response: &proxiima::GetMemoriesReadResponse) -> &[proxiima::MemorySnapshot] {
        &response.memories
    }

    let memory_id = proxiima::MemoryId::new(uuid::Uuid::nil());
    let request = proxiima::GetMemoriesReadRequest {
        memory_ids: vec![memory_id],
    };
    assert_eq!(request.memory_ids, vec![memory_id]);

    let _: fn(&proxiima::GetMemoriesReadResponse) -> &[proxiima::MemorySnapshot] = memories;
}

#[test]
fn spiice_sdk_imports_from_spiice_module() {
    use proxiima::spiice::{
        FactPayload, PgSidecarRegistry, SchemaId, SpiiceBundle, SpiiceRegistry,
    };
    fn _needs_bundle<T: SpiiceBundle>() {}
    fn _needs_fact<T: FactPayload>() {}
    let _ = SchemaId::new("test/schema-v1".to_owned());
    let _ = SpiiceRegistry::new();
    let _ = PgSidecarRegistry::new();
}

#[test]
#[allow(clippy::too_many_lines)]
fn spiice_sdk_constructs_contract_goal_and_session_values() {
    use proxiima::spiice::{
        CounterRule, DbConstraint, EmbedUnit, EmbeddingRecipe, EraseRule, ExportRule, ForgetRule,
        GoalAuthorship, GoalCreateRequest, GoalDependencyRef, GoalEvidenceRef, GoalMotivationRef,
        GoalTopologyWrite, IdempotencyKey, KeyShape, LanguagePolicy, PayloadKind, ProjectionDecl,
        Provenance, SchemaContract, SchemaRef, SearchProjectionDecl, SidecarAtom,
        SidecarSessionRead, SpiiceContract, SpiiceDescriptor, SpiiceProvenance, Surface,
        TransferRule,
    };

    const SURFACE: Surface = Surface {
        table: "sdk_test.rows",
        key: KeyShape::OwnerId,
        owner_column: Some("owner_id"),
        transfer: TransferRule::StaysOnKey,
        erase: EraseRule::ByOwner,
        export: ExportRule::Rows,
        forget: ForgetRule::Keep { why: "fixture" },
        lexical_language_column: None,
        counter: CounterRule::Counted("rows"),
        completeness: None,
    };
    const SCHEMA: SchemaContract = SchemaContract {
        id: SchemaRef::new("sdk_test", "goal", 1),
        kind: PayloadKind::Goal,
        sidecar_table: None,
        search: SearchProjectionDecl::None {
            why: "fixture",
            tag_column: None,
        },
        embedding: EmbeddingRecipe::Never { why: "fixture" },
        transfer: TransferRule::StaysOnKey,
        provenance: Provenance::None,
        surfaces: &[],
        natural_key_columns: proxiima_core::NaturalKeyDeclaration::EMPTY,
    };
    const CONTRACT: SpiiceContract = SpiiceContract {
        retired_facts: &[],
        spiice_id: "sdk_test",
        ordinal: 7,
        schemas: &[SCHEMA],
        state_surfaces: &[SURFACE],
        scopes: &[],
        kernel_surfaces: &[],
        tools: &[],
        resources: &[],
        projection: ProjectionDecl::None { why: "fixture" },
        bespoke_erase_legs: &[],
        bespoke_transfer_legs: &[],
    };

    let mut registry = proxiima::spiice::SpiiceRegistry::new();
    registry
        .try_add_goal_schema::<SdkGoalPayload>()
        .expect("typed Goal schema is registered through the public path");
    registry
        .try_add_spiice(SpiiceDescriptor {
            spiice_id: "sdk_test".to_owned(),
            display_name: "SDK test".to_owned(),
            package_version: "0.1.0".to_owned(),
            author: None,
            provenance: SpiiceProvenance::Builtin,
        })
        .expect("descriptor is accepted");
    registry
        .try_add_contract(&CONTRACT)
        .expect("contract is accepted");
    let frozen = registry.try_freeze().expect("facade contract freezes");
    assert_eq!(
        frozen.spiice_contract("sdk_test").unwrap().schemas[0].id,
        SCHEMA.id
    );

    let owner = proxiima::spiice::OwnerId::from(proxiima::spiice::UserId::new(uuid::Uuid::nil()));
    let memory = proxiima::spiice::MemoryId::new(uuid::Uuid::now_v7());
    let goal = proxiima::spiice::GoalId::new(uuid::Uuid::now_v7());
    let topology = GoalTopologyWrite::new(
        vec![GoalMotivationRef::perspective(memory)],
        vec![GoalDependencyRef::new(goal)],
        vec![GoalEvidenceRef::new(memory)],
    )
    .expect("nested goal topology builds");
    let request = GoalCreateRequest {
        owner,
        topology,
        title: "title".into(),
        text: "text".into(),
        payload: SdkGoalPayload {
            key: "sdk-goal".to_owned(),
        },
        request_id: IdempotencyKey::new("sdk-test").expect("key builds"),
        authorship: GoalAuthorship::User,
        author_self_perspective_id: None,
    };
    assert_eq!(request.text, "text");
    let outcome: Option<proxiima::spiice::GoalWriteOutcome> = None;
    let authorship = GoalAuthorship::System(proxiima::spiice::SystemOrigin::Tool {
        tool_id: proxiima::spiice::ToolId::new("sdk_test/tool"),
    });
    assert!(matches!(authorship, GoalAuthorship::System(_)));
    assert!(outcome.is_none());

    let predicate = [("kind", SidecarAtom::Text("opaque".to_owned()))];
    let read = SidecarSessionRead {
        table: "sdk_test.rows",
        predicates: &predicate,
        limit: Some(1),
    };
    assert_eq!(read.limit, Some(1));
    let _ = (
        LanguagePolicy::Pinned("simple"),
        EmbedUnit {
            column: "text",
            slot: proxiima::spiice::SLOT_DEFAULT,
        },
        DbConstraint {
            relation: "sdk_test.rows",
            name: "rows_owner",
        },
    );
}

#[test]
fn spiice_sdk_exposes_tool_authoring_and_metadata() {
    use proxiima::spiice::{
        McpActionArgSpec, McpAuthorContext, Replay, Tool, ToolCtx, ToolEffect, ToolError,
    };
    fn needs_tool<T: Tool>() {}
    needs_tool::<TierLabelledTool>();
    let _: &[McpActionArgSpec] = &[];
    let _: Option<(&ToolCtx, &ToolError, &McpAuthorContext)> = None;
    let _: ToolEffect = ToolEffect::Additive(Replay::Idempotent);
}

/// A spiice tool that accepts its own `model_id`, written against the
/// transport-neutral [`Tool`](proxiima::spiice::Tool) trait — the shape the
/// operator-label rule has to be reachable from.
struct TierLabelledTool;

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct TierLabelledArgs {
    #[serde(default)]
    model_id: Option<String>,
}

#[derive(serde::Serialize, schemars::JsonSchema)]
struct TierLabelledOutput {
    label: String,
}

impl proxiima::spiice::Tool for TierLabelledTool {
    const TITLE: &'static str = "Test tool";
    const NAME: &'static str = "tier_labelled";
    const DESCRIPTION: &'static str = "resolve an operator label from a spiice tool";

    type Args = TierLabelledArgs;
    type Output = TierLabelledOutput;

    fn call(
        ctx: proxiima::spiice::ToolCtx,
        args: Self::Args,
    ) -> futures::future::BoxFuture<'static, Result<Self::Output, proxiima::spiice::ToolError>>
    {
        // The point of the test: `Tool::call` is handed a `ToolCtx`, whose
        // fields are private and which cannot produce an `McpToolCtx`. If
        // this does not compile, an out-of-tree tool that takes `model_id`
        // has no way to honour a token-bound model identity.
        Box::pin(async move {
            Ok(TierLabelledOutput {
                label: ctx.operator_label(args.model_id.as_deref())?,
            })
        })
    }
}

/// The operator-label rule must be reachable from *both* tool traits, or a
/// spiice that takes its own `model_id` has to reimplement it — and a
/// reimplementation is how a nested label got past the edge in the first
/// place.
#[test]
fn spiice_sdk_exposes_the_operator_label_rule_and_host_adapter() {
    use proxiima::host::{McpToolCtx, McpToolError};
    use proxiima::spiice::{ToolCtx, ToolError, TrustedModelIdError};

    fn needs_tool<T: proxiima::spiice::Tool>() {}
    needs_tool::<TierLabelledTool>();

    // The `McpTool` spelling: a free function over the MCP context.
    let _: fn(&McpToolCtx, Option<&str>) -> Result<String, McpToolError> =
        proxiima::host::operator_label;
    // The `Tool` spelling: an inherent method on the neutral context.
    let _: fn(&ToolCtx, Option<&str>) -> Result<String, ToolError> = ToolCtx::operator_label;
    // Binding provenance onto the caller copy is fallible for the same
    // reason it is on `AuthzContext`.
    let _: Option<TrustedModelIdError> = None;
}

/// A stateful Fact declared through the SDK alone: one hot head per natural
/// key, plus catalog metadata identifying which payload value means "gone".
/// Query still returns that deletion-observation head.
#[derive(serde::Serialize, serde::Deserialize)]
struct TierStatefulFact {
    slot: String,
    state: String,
}

impl proxiima::spiice::FactPayload for TierStatefulFact {
    const SCHEMA_ID: &'static str = "proxiima-tier/stateful-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn receipt_key(&self) -> Vec<u8> {
        let mut key =
            proxiima::spiice::PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
        key.field_str("slot", &self.slot);
        key.field_str("state", &self.state);
        key.finish()
    }

    fn render(&self) -> String {
        String::new()
    }

    fn sidecar_table() -> Option<&'static str> {
        Some("proxiima_tier.stateful_v1")
    }

    fn tombstone() -> Option<proxiima::spiice::FactTombstone> {
        Some(proxiima::spiice::FactTombstone {
            column: "state",
            value: "Tombstone",
        })
    }
}

impl proxiima::spiice::PayloadProjection for TierStatefulFact {
    const KEY_FIELDS: &'static [proxiima::spiice::KeyField<Self>] =
        &[proxiima::spiice::KeyField::text("slot", |payload| {
            &payload.slot
        })];
    fn display(&self) -> proxiima::spiice::PayloadDisplay {
        proxiima::spiice::PayloadDisplay::default()
    }
    fn durable(&self) -> Result<proxiima::spiice::DurablePayload, String> {
        let mut builder = proxiima::spiice::DurablePayloadBuilder::new();
        builder
            .field("slot", &self.slot)?
            .field("state", &self.state)?;
        Ok(builder.finish())
    }
}

#[test]
fn spiice_sdk_exposes_the_stateful_fact_tombstone() {
    use proxiima::spiice::{FactPayload, FactTombstone};

    // Naming the override's return type is the load-bearing half: without
    // `FactTombstone` on the facade the signature above is unspellable, and
    // an out-of-tree spiice cannot expose its deletion discriminator.
    let tombstone: Option<FactTombstone> = <TierStatefulFact as FactPayload>::tombstone();
    let tombstone = tombstone.expect("the schema declares a tombstone discriminator");
    assert_eq!(tombstone.column, "state");
    assert_eq!(tombstone.value, "Tombstone");

    // The other half of the stateful contract travels with it.
    assert_eq!(
        <TierStatefulFact as proxiima::spiice::PayloadProjection>::KEY_FIELDS
            .iter()
            .map(proxiima::spiice::KeyField::name)
            .collect::<Vec<_>>(),
        vec!["slot"]
    );
}

#[test]
fn spiice_sdk_exposes_the_cited_blob_lane() {
    // A worker resolves this port from `SpiiceServices`, so both the handle
    // and the trait behind it must be nameable from `proxiima::spiice` alone.
    use proxiima::spiice::{
        CitedBlobHeld, CitedBlobIntegrityMismatch, CitedBlobOwnerMissingObject,
        CitedBlobOwnerReconcileOutcome, CitedBlobOwnerReconcilePort,
        CitedBlobOwnerReconcileService, CitedBlobPort, CitedBlobReadError, CitedBlobReadPort,
        CitedBlobReadService, CitedBlobReadUrl, CitedBlobService, CitedBlobStaged,
        CitedBlobUploadAborted, CitedBlobUploadCompleted, CitedBlobUploadHeader,
        CitedBlobUploadPrepared, MAX_HELD_BLOB_DIGESTS, UploadCompletionExpectation,
        UploadedBlobPayload, VerifiedCitedBlob,
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

    let expectation =
        UploadCompletionExpectation::new([3u8; 32], 17, "application/pdf", "handbuch.pdf");
    assert_eq!(expectation.content_hash(), &[3u8; 32]);
    assert_eq!(expectation.byte_len(), 17);
    assert_eq!(expectation.mime(), "application/pdf");
    assert_eq!(expectation.filename(), "handbuch.pdf");

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
    // merely name it — and a spiice that only ever received these would
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
    // proves nothing. What needs proving is that a spiice can reach it to
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
    let owner: proxiima::OwnerId = proxiima::company_owner(uuid::Uuid::nil());
    // The subject is a different id from the group: an owner id equal to the
    // subject's own is its personal owner and takes no override.
    let _authz: proxiima::AuthzContext = proxiima::AuthzContext::for_subject_with_role(
        proxiima::UserId::new(uuid::Uuid::max()),
        [(owner, proxiima::Role::admin())],
        proxiima::AuthPath::HostBearer,
    );

    // `Role::new` / `Role::may_write` / `OwnerRoles::for_subject` name these.
    // Hosts that only imported `Role` had to take `proxiima-core` to spell them.
    let ceiling = proxiima::AccessCeiling::Fact;
    let role = proxiima::Role::new(ceiling, ceiling, false).expect("write <= read");
    assert!(role.may_write(proxiima::AccessKind::Fact));
    let user = proxiima::UserId::new(uuid::Uuid::max());
    let group = proxiima::OwnerId::from(proxiima::GroupId::new(uuid::Uuid::nil()));
    let _roles = proxiima::OwnerRoles::for_subject(user, [(group, role)]).expect("group roles");
    let _: Option<proxiima::AccessError> = None;
}

#[test]
fn writer_attribution_is_shared_by_host_and_spiice_facades() {
    fn copy_writer(scope: &proxiima::OwnerScope) -> proxiima::host::WriterAttribution {
        let mut copy = scope.writer();
        copy.via = None;
        copy
    }

    let writer: proxiima::host::WriterAttribution = proxiima::spiice::WriterAttribution {
        by: uuid::Uuid::from_u128(1),
        via: Some(uuid::Uuid::from_u128(2)),
    };
    let _: proxiima::WriterAttribution = writer;
    let _: fn(&proxiima::OwnerScope) -> proxiima::spiice::WriterAttribution =
        proxiima::OwnerScope::writer;
    std::hint::black_box(copy_writer);
}

/// A spiice-owned Abstraction, defined through the SDK alone. Note
/// `sidecar_table` is required rather than optional on this trait: unlike
/// a Fact, a derived memory always has a typed sidecar, so a spiice
/// registering one always owns a migration for it too.
#[derive(serde::Serialize, serde::Deserialize)]
struct TierAbstraction {
    note: String,
}

impl proxiima::spiice::AbstractionPayload for TierAbstraction {
    const SCHEMA_ID: &'static str = "proxiima-tier/abstraction-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxiima_tier.abstraction_v1"
    }
}

impl proxiima::spiice::PayloadProjection for TierAbstraction {
    const KEY_FIELDS: &'static [proxiima::spiice::KeyField<Self>] = &[];
    fn display(&self) -> proxiima::spiice::PayloadDisplay {
        proxiima::spiice::PayloadDisplay {
            body: Some(self.note.clone()),
            ..Default::default()
        }
    }
    fn durable(&self) -> Result<proxiima::spiice::DurablePayload, String> {
        let mut builder = proxiima::spiice::DurablePayloadBuilder::new();
        builder.field("note", &self.note)?;
        Ok(builder.finish())
    }
}

#[test]
fn spiice_sdk_exposes_the_derived_memory_write_lane() {
    // `AbstractionPayload` and `PerspectivePayload` let a spiice *declare*
    // derived schemas; `DerivedMemory` is the typed write request.
    // A spiice with a direct `proxiima-storage-pg` dependency reaches the
    // same lane through it, which a spiice depending only on `proxiima`
    // does not have.
    //
    // This constructs the request rather than merely naming the types, so
    // a field added, removed or retyped upstream breaks here instead of
    // silently breaking every out-of-tree spiice at its next pin bump.
    use proxiima::spiice::{
        DerivationIdentity, DerivedMemory, DerivedMemoryOutcome, InputContractId, MemoryId,
        MemoryTarget, OperatorId, SeriesHandle,
    };

    let owner: proxiima::OwnerId = proxiima::company_owner(uuid::Uuid::nil());
    let derived = MemoryId::new(uuid::Uuid::nil());
    let source_fact = MemoryId::new(uuid::Uuid::nil());

    let _req = DerivedMemory::abstraction(
        MemoryTarget::Series(SeriesHandle::new(derived.into_inner())),
        owner,
        "the text a derived memory is embedded from",
        TierAbstraction {
            note: "sidecar".to_owned(),
        },
        [source_fact],
        DerivationIdentity::new(
            OperatorId::new(uuid::Uuid::nil()),
            InputContractId::new(uuid::Uuid::nil()),
        ),
    )
    .expect("typed derived request builds");

    // The outcome type must be nameable too — a caller has to bind what
    let _: Option<&DerivedMemoryOutcome> = None;
}

/// A payload that points at another node, built through the facade alone.
///
/// This is the whole of how an out-of-tree spiice creates a connection: it
/// declares reference fields, ingest reads them, and the index rows follow.
/// If [`PayloadReference`] or its binding vocabulary were missing from the
/// facade, the override could not be written at all and a spiice's schemas
/// could only ever be islands.
#[test]
fn spiice_sdk_exposes_the_payload_reference_lane() {
    use proxiima::spiice::{EdgeKind, EntityKind, MemoryId, PayloadReference, ReferenceBinding};

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

    // A kind is read, never written: it is what an edge IS, decided by the
    // write that produced the row.
    assert_eq!(EdgeKind::Reference.as_str(), "reference");
    assert_eq!(EdgeKind::Origin.as_str(), "origin");
}

/// A spiice-owned service, standing in for the store a real spiice hands
/// its tools (the example spiice puts its `ExampleStore` here). Core cannot
/// name this type, which is the entire point of the service map.
struct TierSpiiceStore {
    marker: &'static str,
}

/// The override a spiice host must be able to *write*. This is the load-
/// bearing half: `services` returns `SpiiceServices`, so
/// without that type on the spiice facade the signature is unspellable and
/// a spiice depending only on `proxiima` cannot supply its tools with a
/// database handle or any other host-owned dependency.
struct TierExtensionApp;

impl proxiima::spiice::SpiiceBundle for TierExtensionApp {
    fn register(
        _registry: &mut proxiima::spiice::SpiiceRegistry,
    ) -> Result<(), proxiima::spiice::SpiiceRegistryError> {
        Ok(())
    }

    fn migrators() -> Vec<proxiima::spiice::NamedMigrator> {
        Vec::new()
    }
}

impl proxiima::SpiiceApp for TierExtensionApp {
    fn app_info() -> proxiima::AppInfo {
        proxiima::AppInfo {
            id: "proxiima-tier",
            title: "Tier Extension App",
            version: "0",
        }
    }

    fn services(
        ctx: &proxiima::AppContext,
    ) -> Result<proxiima::spiice::SpiiceServices, proxiima::spiice::SpiiceServiceError> {
        // A real host composes its store from `ctx.clone_pool_for_host()`.
        // This test only pins that the override is writable; the bridge
        // itself is named in `host_extra_table_bridge_is_on_app_context`.
        let _ = ctx;
        let mut services = proxiima::spiice::SpiiceServices::default();
        services.try_insert(TierSpiiceStore { marker: "store" })?;
        Ok(services)
    }
}

#[test]
fn spiice_sdk_exposes_the_spiice_service_seam() {
    use proxiima::spiice::{SpiiceServiceError, SpiiceServices};

    // Both halves must be reachable: the host inserts a service core
    // cannot name, and the tool resolves it back by type.
    let mut services = SpiiceServices::default();
    services
        .try_insert(TierSpiiceStore { marker: "store" })
        .unwrap();
    let resolved = services
        .get::<TierSpiiceStore>()
        .expect("a tool must resolve the service its host inserted");
    assert_eq!(resolved.marker, "store");

    // The one-shot constructor is the common case for a single service.
    let single = SpiiceServices::with(TierSpiiceStore { marker: "single" });
    assert_eq!(
        single
            .get::<TierSpiiceStore>()
            .expect("with() inserts the value")
            .marker,
        "single"
    );

    // An absent service resolves to None rather than panicking.
    assert!(single.get::<u64>().is_none());

    // And the override itself is nameable; calling it needs an AppContext
    // only a booted runtime can make, so this pins the signature.
    std::hint::black_box(
        <TierExtensionApp as proxiima::SpiiceApp>::services
            as fn(&proxiima::AppContext) -> Result<SpiiceServices, SpiiceServiceError>,
    );
}

#[test]
fn host_api_can_configure_the_blob_lane_without_the_environment() {
    // `Proxiima::s3` is a `pub` method and `BuiltProxiima::blobs` is a `pub`
    // field of type `Option<CitedBlobStore>`, so both types were already
    // part of the public surface — they just could not be NAMED from
    // `proxiima`. A host could reach them by inference and could not write
    // either in a signature, hold one in a struct, or configure S3 at all
    // except through `S3RuntimeConfig::from_env`, which makes process
    // environment a hard requirement of a library API.
    // Nameable in a signature, which is what "the field can be held" means.
    fn _holds_the_lane(_lane: Option<proxiima::CitedBlobStore>) {}

    let config = proxiima::S3RuntimeConfig {
        bucket: "some-bucket".to_owned(),
        region: "us-east-1".to_owned(),
        key_pair: proxiima::S3KeyPair::new("test-access-key".into(), "test-secret-key".into()),
        endpoint_url: Some("http://localhost:9000".to_owned()),
        public_endpoint_url: None,
        plaintext_hosts: proxiima::host::PlaintextHosts::none(),
        force_path_style: true,
        upload_ttl_seconds: 900,
        read_ttl_seconds: 300,
        max_blob_bytes: None,
        store_id: None,
    };
    assert_eq!(config.bucket, "some-bucket");

    // The builder method that takes it is writable, which is the point.
    std::hint::black_box(
        proxiima::Proxiima::<TierExtensionApp>::s3
            as fn(
                proxiima::Proxiima<TierExtensionApp>,
                proxiima::S3RuntimeConfig,
            ) -> proxiima::Proxiima<TierExtensionApp>,
    );
}

#[test]
fn host_api_can_configure_the_pg_pool_without_process_environment() {
    let config = proxiima::PgPoolConfig {
        max_connections: 4,
        statement_timeout: std::time::Duration::from_secs(30),
        acquire_timeout: std::time::Duration::from_secs(2),
        idle_timeout: std::time::Duration::from_mins(1),
        max_lifetime: std::time::Duration::from_mins(2),
    };
    assert_eq!(config.max_connections, 4);

    std::hint::black_box(
        proxiima::RuntimeBuilder::pg_pool_config
            as fn(proxiima::RuntimeBuilder, proxiima::PgPoolConfig) -> proxiima::RuntimeBuilder,
    );
    std::hint::black_box(
        proxiima::Proxiima::<TierExtensionApp>::pg_pool_config
            as fn(
                proxiima::Proxiima<TierExtensionApp>,
                proxiima::PgPoolConfig,
            ) -> proxiima::Proxiima<TierExtensionApp>,
    );
}

#[test]
fn host_api_names_global_and_owner_reconcile_outcomes() {
    // `CitedBlobStore::reconcile_all` is a `pub async fn` reachable
    // through `AppContext::blobs` / `BuiltProxiima::blobs` /
    // `RunningProxiima::blobs`. Calling it also requires the booted runtime's
    // same-boot, non-Clone `SystemAuthority`. The owner service is a distinct,
    // authorization-carrying lane with a locator-free result.
    use proxiima::{
        CitedBlobMissingObject, CitedBlobOwnerMissingObject, CitedBlobOwnerReconcileOutcome,
        CitedBlobOwnerReconcileService, CitedBlobReadError, CitedBlobReadService,
        CitedBlobReconcileOutcome, MAX_RECONCILE_SAMPLE, VerifiedCitedBlob,
    };

    // Nameable in a signature, which is what "the return type can be held"
    // means.
    fn _returns_the_outcome() -> proxiima::CitedBlobReconcileOutcome {
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

    // `#[non_exhaustive]`: a host starts from the default and sets fields.
    let mut outcome = CitedBlobReconcileOutcome::default();
    outcome.rows_scanned = 1;
    outcome.missing_objects = 1;
    outcome.missing_sample = vec![missing];
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
    // `SpiiceRegistryFrozen::list_mcp_tools` already returns this slice.
    // The element type and origin enum must be nameable on the Host API.
    use proxiima::host::{McpToolDescriptor, McpToolOrigin, SpiiceRegistryFrozen};

    fn bind(frozen: &SpiiceRegistryFrozen) -> &[McpToolDescriptor] {
        frozen.list_mcp_tools()
    }

    let frozen = proxiima::spiice::SpiiceRegistry::new()
        .try_freeze()
        .expect("empty registry freezes");
    let named = bind(&frozen)
        .iter()
        .filter(|tool| {
            matches!(
                tool.origin,
                McpToolOrigin::Substrate | McpToolOrigin::Spiice(_)
            )
        })
        .count();
    assert!(
        named >= 1,
        "substrate tools register on SpiiceRegistry::new"
    );
}

#[test]
fn host_api_binds_an_owner_as_its_bare_id() {
    // A spiice with its own tables binds an owner as one uuid column. The
    // id is the whole identity: a user and a group with one uuid are one
    // owner, and the kind lives only in the core registry.
    let group = proxiima::GroupId::new(uuid::Uuid::nil());
    let user = proxiima::UserId::new(uuid::Uuid::nil());
    assert_eq!(
        proxiima::OwnerId::from(group).into_inner(),
        uuid::Uuid::nil()
    );
    assert_eq!(
        proxiima::OwnerId::from(user),
        proxiima::OwnerId::from(group)
    );
    let parsed: proxiima::OwnerId = uuid::Uuid::nil().to_string().parse().expect("bare uuid");
    assert_eq!(parsed, proxiima::OwnerId::from(user));
}

#[test]
fn raw_storage_surfaces_are_not_supported_tier_exports() {
    let host_exports = include_str!("../src/host.rs");
    let spiice_exports = include_str!("../src/spiice.rs");
    let authorized_read = include_str!("../src/spiice/authorized_read.rs");

    // `PgPoolConfig` is pure policy, not the raw SQLx handle this guard bans.
    assert!(!host_exports.replace("PgPoolConfig", "").contains("PgPool"));
    // `PgStorage` is exported once, as the parameter of the migration helpers
    // and the host accessors (#413); no other line of host.rs names it, so its
    // backend verbs are not republished by a second route.
    let naming_pg_storage: Vec<&str> = host_exports
        .lines()
        .filter(|line| !line.trim_start().starts_with("//") && line.contains("PgStorage"))
        .collect();
    assert_eq!(
        naming_pg_storage,
        ["pub use proxiima_storage_pg::PgStorage;"]
    );
    assert!(!host_exports.contains("StorageHandle"));
    assert!(!spiice_exports.contains("PgPool"));
    assert!(!spiice_exports.contains("PgStorage"));
    assert!(!spiice_exports.contains("StorageHandle"));
    assert!(
        !authorized_read.contains("use sqlx::PgPool") && !authorized_read.contains("&PgPool"),
        "code-series pool helpers must not live on the Spiice SDK"
    );
}

#[test]
fn host_extra_table_bridge_is_on_proxiima_host() {
    // The one sanctioned PgPool leak is ProxiimaHost::clone_pool_for_host,
    // reached through `AppContext::host` (docs/host-integration.md). Spiice SDK still must
    // not name the type. The host accessors are scanned so this cannot hide
    // in a submodule the host.rs scan misses.
    let spiice_exports = include_str!("../src/spiice.rs");
    let authorized_read = include_str!("../src/spiice/authorized_read.rs");
    let host = include_str!("../src/proxiima_host.rs");

    assert!(!spiice_exports.contains("PgPool"));
    assert!(
        !authorized_read.contains("use sqlx::PgPool") && !authorized_read.contains("&PgPool"),
        "code-series pool helpers must not live on the Spiice SDK"
    );
    assert!(
        host.contains("pub fn clone_pool_for_host"),
        "host extra-table bridge must stay on ProxiimaHost"
    );
    assert!(
        !host.contains("pub pool"),
        "the pool field stays crate-private"
    );
    let _: fn(&proxiima::ProxiimaHost) -> sqlx::PgPool =
        proxiima::ProxiimaHost::clone_pool_for_host;
    let _: fn(&proxiima::ProxiimaHost) -> proxiima::PgTuning =
        proxiima::ProxiimaHost::pg_tuning_for_host;
    let _: fn(&proxiima::AppContext) -> &proxiima::ProxiimaHost = proxiima::AppContext::host;
}

/// #389: one set of host accessors, reached the same way from every handle
/// on a booted runtime.
#[test]
fn every_runtime_handle_reaches_the_one_host_accessor_set() {
    let _: fn(&proxiima::AppContext) -> &proxiima::ProxiimaHost = proxiima::AppContext::host;
    let _: fn(&proxiima::BuiltProxiima) -> &proxiima::ProxiimaHost = proxiima::BuiltProxiima::host;
    let _: fn(&proxiima::RunningProxiima) -> &proxiima::ProxiimaHost =
        proxiima::RunningProxiima::host;
    let _: fn(&proxiima::ProxiimaHost) -> proxiima::PgPlatformScope =
        proxiima::ProxiimaHost::platform_scope_for_host;
    let _: fn(&proxiima::ProxiimaHost) -> proxiima::PgHostStateEraseContext =
        proxiima::ProxiimaHost::host_state_erase_context_for_host;
    let _: fn(&proxiima::ProxiimaHost) -> proxiima::OriginScope =
        proxiima::ProxiimaHost::origin_scope_for_host;
}

/// #413: the types the host accessors return are the facade's own names, and
/// the storage handle and the owner-scoped transaction are host-tier only.
#[test]
fn host_tier_names_the_storage_and_publication_types_its_signatures_expose() {
    async fn adapter_transaction(
        pool: &sqlx::PgPool,
        scope: &proxiima::OwnerScope,
    ) -> Result<sqlx::Transaction<'static, sqlx::Postgres>, proxiima::StorageError> {
        proxiima::begin_owner_transaction(pool, scope).await
    }
    fn is_eligibility_port<P: proxiima::PublicationOriginEligibilityPort>() {}

    let _: fn(
        &proxiima::PgStorage,
    ) -> Result<proxiima::PgHostStateEraseContext, proxiima::StorageError> =
        proxiima::PgStorage::host_state_erase_context;
    std::hint::black_box(adapter_transaction);
    is_eligibility_port::<proxiima::PgStorage>();
    let _: Option<proxiima::PublicationOriginEligibility> = None;
}

/// #413: `EraseRule::HostState` is built from the Spiice SDK alone, and the
/// SDK's name is the host tier's name.
#[test]
fn the_spiice_sdk_names_the_host_state_erase_disposition() {
    use proxiima::spiice::{EraseRule, HostStateEraseDisposition};

    let rule = EraseRule::HostState {
        whole_owner: HostStateEraseDisposition::Erase,
        source: HostStateEraseDisposition::Retain,
        exact_fact: HostStateEraseDisposition::Erase,
    };
    let EraseRule::HostState { source, .. } = rule else {
        unreachable!("built above");
    };
    let host_tier: proxiima::HostStateEraseDisposition = source;
    assert_eq!(host_tier, proxiima::HostStateEraseDisposition::Retain);
}

/// #389: the embedded builder, its config and boot result, the host-called
/// spawners and the MCP enable flag are gone, not deprecated.
#[test]
fn the_second_builder_and_the_host_spawners_are_not_public() {
    let sources = [
        include_str!("../src/lib.rs"),
        include_str!("../src/host.rs"),
        include_str!("../src/runtime.rs"),
        include_str!("../src/runtime_config.rs"),
        include_str!("../src/config.rs"),
        include_str!("../src/boot.rs"),
        include_str!("../src/features.rs"),
        include_str!("../src/app.rs"),
        include_str!("../src/proxiima_host.rs"),
    ];
    for removed in [
        "ProxiimaBuilder",
        "EmbeddedProxiima",
        "EmbedConfig",
        "EmbedError",
        "fn spawn_embedding_worker(&self",
        "pub fn spawn_publication_publisher",
        "pub fn spawn_publication_copy_cleaner",
        "fn with_mcp",
    ] {
        for source in sources {
            assert!(
                !source.contains(removed),
                "`{removed}` is back on the facade"
            );
        }
    }
}

#[test]
fn host_state_participation_is_host_api_not_spiice_sdk() {
    let spiice_exports = include_str!("../src/spiice.rs");
    let host_exports = include_str!("../src/host.rs");
    assert!(
        !spiice_exports.contains("HostStateCommand"),
        "Spiice SDK must not export host-state command types"
    );
    assert!(
        !spiice_exports.contains("PgHostStateParticipant"),
        "Spiice SDK must not export the PG host-state participant"
    );
    assert!(
        host_exports.contains("HostStateCommand"),
        "Host API names HostStateCommand"
    );
    assert!(
        host_exports.contains("PgHostStateParticipant"),
        "Host API names PgHostStateParticipant"
    );
    for dispatch in [
        "PgCommandDispatcher",
        "AgreedCommand",
        "HostStateHandler",
        "HostStatePayloadOwners",
        "CommandRegistrationError",
    ] {
        assert!(
            !spiice_exports.contains(dispatch),
            "Spiice SDK must not export `{dispatch}`, the host-state dispatch"
        );
        assert!(
            host_exports.contains(dispatch),
            "Host API names `{dispatch}`"
        );
    }
}

/// Naming [`proxiima::spiice::AuthorizationHook`] is not enough: `veto`
/// takes [`proxiima::spiice::AuthzInput`] and returns [`proxiima::spiice::AuthzVeto`].
#[derive(Debug)]
struct TierAuthzHook;

impl proxiima::spiice::AuthorizationHook for TierAuthzHook {
    fn veto(
        &self,
        input: &proxiima::spiice::AuthzInput<'_>,
    ) -> Result<(), proxiima::spiice::AuthzVeto> {
        match &input.operation {
            proxiima::spiice::AuthzOperation::Read { .. }
            | proxiima::spiice::AuthzOperation::Write { .. }
            | proxiima::spiice::AuthzOperation::OwnerAdmin
            | proxiima::spiice::AuthzOperation::Membership {
                change: proxiima::spiice::MembershipChange::Add,
                ..
            }
            | proxiima::spiice::AuthzOperation::EntityTransfer { .. }
            | proxiima::spiice::AuthzOperation::GoalCollaboration { .. } => Ok(()),
            proxiima::spiice::AuthzOperation::Membership {
                change: proxiima::spiice::MembershipChange::Remove,
                ..
            } => Err(proxiima::spiice::AuthzVeto("denied".into())),
        }
    }

    fn observe(
        &self,
        _input: &proxiima::spiice::AuthzInput<'_>,
        outcome: proxiima::spiice::AuthzOutcome,
    ) {
        let _ = matches!(outcome, proxiima::spiice::AuthzOutcome::Allowed);
    }
}

#[derive(Debug)]
struct TierOwnerResolver;

impl proxiima::spiice::OwnerResolver for TierOwnerResolver {
    fn resolve(
        &self,
        _authz: &proxiima::AuthzContext,
        requested: &proxiima::OwnerId,
    ) -> Result<proxiima::OwnerId, proxiima::ProtocolError> {
        Ok(*requested)
    }
}

#[test]
fn spiice_sdk_names_query_and_ingest_types() {
    use proxiima::spiice::{AuthorizationHook, OwnerResolver};

    fn assert_hook<T: AuthorizationHook + ?Sized>() {}
    fn assert_resolver<T: OwnerResolver + ?Sized>() {}
    assert_hook::<dyn AuthorizationHook>();
    assert_resolver::<dyn OwnerResolver>();

    let owner = proxiima::OwnerId::from(proxiima::UserId::new(uuid::Uuid::nil()));
    let authz = proxiima::AuthzContext::single_owner(&owner, proxiima::AuthPath::System);
    let input = proxiima::spiice::AuthzInput {
        authz: &authz,
        requested: &owner,
        resolved: &owner,
        operation: proxiima::spiice::AuthzOperation::Read {
            kind: proxiima::AccessKind::Fact,
        },
    };
    TierAuthzHook.veto(&input).expect("read allow");
    TierAuthzHook.observe(&input, proxiima::spiice::AuthzOutcome::Allowed);
    let transfer = proxiima::spiice::AuthzInput {
        authz: &authz,
        requested: &owner,
        resolved: &owner,
        operation: proxiima::spiice::AuthzOperation::EntityTransfer {
            entity: proxiima::spiice::EntityId::Memory(proxiima::MemoryId::new(uuid::Uuid::nil())),
            to_owner: owner,
        },
    };
    TierAuthzHook.veto(&transfer).expect("transfer allow");
    assert_eq!(
        TierOwnerResolver.resolve(&authz, &owner).expect("resolve"),
        owner
    );

    let mut registry = proxiima::spiice::SpiiceRegistry::new();
    registry.add_authorization_hook(std::sync::Arc::new(TierAuthzHook));
    registry
        .try_set_owner_resolver(std::sync::Arc::new(TierOwnerResolver))
        .expect("one resolver");

    let _ = proxiima::spiice::SidecarAtom::I32(0);
    let _ = proxiima::spiice::CitationSpec::v1("core/upload-v1", [0; 32]);
    let fact = TierStatefulFact {
        slot: "a".into(),
        state: "Present".into(),
    };
    let _cite = proxiima::spiice::FactWrite::new(
        proxiima::spiice::OwnerId::from(proxiima::spiice::UserId::new(uuid::Uuid::nil())),
        "test/src",
        &fact,
    )
    .citation(proxiima::spiice::CitationSpec::v1(
        "core/upload-v1",
        [0; 32],
    ));
    let _: Option<proxiima::spiice::UnitOfWork<'_>> = None;
    let _ = proxiima::spiice::SearchMode::Hybrid;

    let _: proxiima::spiice::QueryRequest = proxiima::spiice::QueryRequest::readable();
    let _: Option<(
        proxiima::spiice::QueryResponse,
        proxiima::spiice::GoalRow,
        proxiima::spiice::FactIngestOutcome,
        proxiima::spiice::FactWriteCommand,
        proxiima::spiice::AuthorizedFactWithCitation,
        proxiima::spiice::AuthorizedFactWithCitationRef,
    )> = None;
    let _cited = proxiima::spiice::InlineCitedObjectDraft {
        schema_id: proxiima::spiice::SchemaId::new("core/upload-v1".into()),
        schema_version: proxiima::spiice::SchemaVersion::new(1),
        payload_bytes: Vec::new(),
    };
}

#[cfg(feature = "auth-oidc")]
#[test]
fn auth_module_names_oidc_primitives() {
    let _: Option<(
        proxiima::auth::OidcAuthConfig,
        proxiima::auth::OidcTokenValidator,
        proxiima::auth::ValidatedOidcClaims,
        proxiima::auth::HttpJwksResolver,
        proxiima::auth::AccessError,
        proxiima::auth::OwnerRoles,
    )> = None;
    let _: fn(
        &proxiima::ProxiimaHost,
        proxiima::spiice::SpiiceServices,
    ) -> Result<proxiima::CoreMcpTools, proxiima::spiice::SpiiceServiceError> =
        proxiima::ProxiimaHost::core_mcp_tools_with_request_services;
}

/// One write path: `Engine` / `UnitOfWork`.
///
/// The storage backend's fact-ingest and derive verbs are the body of its
/// port impls, not an API. If a `pub fn` reappears in either file, some
/// caller can once more begin its own transaction and append a Fact or a
/// derived row past the engine — and past the projection row, the sketch,
/// and the embedding enqueue that the one path maintains with it.
///
/// Scanned rather than expressed in types: a type-level pin cannot name a
/// function that must not exist, so the check has to read the source. `pub
/// trait` / `pub type` / `pub struct` are unaffected — spiices still
/// implement the sidecar traits; they just cannot invoke them (see
/// `SidecarInsertPermit`).
#[test]
fn storage_write_verbs_expose_no_public_function() {
    for (module, source) in [
        (
            "verbs::fact_ingest",
            include_str!("../../storage-pg/src/verbs/fact_ingest.rs"),
        ),
        (
            "verbs::derive_append",
            include_str!("../../storage-pg/src/verbs/derive_append.rs"),
        ),
    ] {
        for (index, line) in source.lines().enumerate() {
            let trimmed = line.trim_start();
            assert!(
                !(trimmed.starts_with("pub fn ") || trimmed.starts_with("pub async fn ")),
                "{module}:{} exposes a public write function; the write tier is \
                 `Engine`/`UnitOfWork` only, so this belongs behind `pub(crate)`: {trimmed}",
                index + 1
            );
        }
    }
}

/// No storage-backend surface hands out a public write onto a transaction.
///
/// Two files, one rule, because they are the same door in two places.
///
/// The frozen registry's `insert_*` methods are the CORRECT door — they write
/// the projection row with the sidecar row — and that is exactly why a public
/// one is still a second write path: correct today is not the same as
/// maintained-by-construction tomorrow. `memory_timeseries` is the row under
/// all of them: a public `ingest_fact_timeseries` lands an admission with no
/// sidecar, no projection row, no sketch and no embedding enqueue, which is
/// every invariant its callers add. Its two transaction-scoped reads go with
/// it — they read the table directly, and the authorized read surface is the
/// query ports.
///
/// Two exemptions, on two different grounds.
///
/// `rebuild_projection_for_table` stays public because rebuild re-derives a
/// projection row FROM a sidecar row and so can only restore the invariant,
/// never break it.
///
/// `integrity_check` stays public because it is not a read of the data at
/// all: every statement it runs is a `SELECT count(*)` over a registered
/// surface, so what crosses the crate boundary is an integer per surface —
/// no row, no payload, no owner, nothing an authorized read would have had
/// to scope. It is public for the reason it exists: a downstream spiice's
/// CI has to be able to assert zero drift after its own ingest suite, and a
/// check nobody outside the crate can run is a check nobody runs.
///
/// The rule is "public fn taking a transaction, connection, or pool", not
/// "public fn": the registry's read and lookup methods
/// (`tables_for_payloads`, `memory_sidecar_tables`,
/// `load_memory_payloads_batch`, …) are how the engine and the inverse lanes
/// ask it questions.
#[test]
fn storage_backend_exposes_no_public_transaction_taking_fn() {
    const CONNECTION_TYPES: [&str; 3] = ["Transaction", "PgConnection", "PgPool"];
    let surfaces: [(&str, &str, &[&str]); 2] = [
        (
            "sidecars/frozen.rs",
            include_str!("../../storage-pg/src/sidecars/frozen.rs"),
            &["rebuild_projection_for_table", "integrity_check"],
        ),
        (
            "verbs/memory_timeseries.rs",
            include_str!("../../storage-pg/src/verbs/memory_timeseries.rs"),
            &[],
        ),
    ];

    for (module, source, exempt) in surfaces {
        let mut lines = source.lines().enumerate();
        while let Some((index, line)) = lines.next() {
            let trimmed = line.trim_start();
            let Some(rest) = trimmed
                .strip_prefix("pub async fn ")
                .or_else(|| trimmed.strip_prefix("pub fn "))
            else {
                continue;
            };
            let name = rest
                .split(['(', '<'])
                .next()
                .unwrap_or(rest)
                .trim()
                .to_owned();
            // A signature runs to the line closing its parameter list; a
            // one-line signature closes on its own line.
            let mut signature = line.to_owned();
            if !line.contains(')') {
                for (_, more) in lines.by_ref() {
                    signature.push_str(more);
                    if more.trim_start().starts_with(')') {
                        break;
                    }
                }
            }
            let takes_connection = CONNECTION_TYPES
                .iter()
                .any(|needle| signature.contains(needle));
            assert!(
                !takes_connection || exempt.contains(&name.as_str()),
                "{module}:{} exposes `{name}` publicly with a transaction, connection, or \
                 pool: a storage write or direct-table read reachable outside this crate is a \
                 second path past the engine. Make it `pub(crate)`; the exemptions are \
                 `rebuild_projection_for_table`, because rebuild IS the maintenance, and \
                 `integrity_check`, because it returns counts and never a row.",
                index + 1
            );
        }
    }
}

#[test]
fn spiice_api_names_plaintext_hosts_and_the_host_tier_carries_them() {
    use proxiima::spiice::{
        EndpointUrlError, EndpointUrlPolicy, PlaintextHosts, validate_endpoint_url_for_hosts,
    };

    let named = PlaintextHosts::parse("gateway").expect("exact host name");
    validate_endpoint_url_for_hosts(
        "http://gateway:9090",
        EndpointUrlPolicy::AllowLoopbackHttp,
        &named,
    )
    .expect("the named host may use plaintext");
    for endpoint in ["http://other:9090", "http://gateway@evil.example"] {
        assert_eq!(
            validate_endpoint_url_for_hosts(endpoint, EndpointUrlPolicy::AllowLoopbackHttp, &named),
            Err(EndpointUrlError::InsecureTransport),
            "{endpoint}",
        );
    }
    assert!(PlaintextHosts::parse("*").is_err());
    let _: proxiima::host::PlaintextHosts = PlaintextHosts::none();
}
