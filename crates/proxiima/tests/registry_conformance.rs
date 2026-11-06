use proxiima::spiice::{
    FactPayload, PayloadKeyBuilder, SpiiceBundle, SpiiceDescriptor, SpiiceProvenance,
    SpiiceRegistry, SpiiceRegistryError, SpiiceRegistryFrozen,
};
// Contract vocabulary is not on the `proxiima` facade (docs/protocol.md tiers), so a
// declaration is spelled against the substrate crate the way the in-tree
// spiice does.
use proxiima::{AppInfo, RuntimeBuilder, SpiiceApp};
use proxiima_core::spiice::{
    EmbeddingRecipe, ProjectionDecl, Provenance, SchemaContract, SchemaEvolution, SchemaRef,
    SearchProjectionDecl, SpiiceContract, TransferRule,
};
use proxiima_core::verbs::schema::PayloadKind;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ConformanceFactV1 {
    value: String,
}

impl FactPayload for ConformanceFactV1 {
    // `SchemaRef` renders `<spiice>/<name>-v<version>`, so the id and the
    // version are one fact: a contract cannot name a schema whose id
    // spells a different version than it declares.
    const SCHEMA_ID: &'static str = "proxiima-conformance/fact-v7";
    const SCHEMA_VERSION: u32 = 7;

    fn receipt_key(&self) -> Vec<u8> {
        let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
        key.field_str("value", &self.value);
        key.finish()
    }

    fn render(&self) -> String {
        self.value.clone()
    }

    fn sidecar_table() -> Option<&'static str> {
        Some("proxiima_conformance.fact_v1")
    }
}

impl proxiima::spiice::PayloadProjection for ConformanceFactV1 {
    const KEY_FIELDS: &'static [proxiima::spiice::KeyField<Self>] = &[];
    fn display(&self) -> proxiima::spiice::PayloadDisplay {
        proxiima::spiice::PayloadDisplay {
            body: Some(self.value.clone()),
            ..Default::default()
        }
    }
    fn durable(&self) -> Result<proxiima::spiice::DurablePayload, String> {
        let mut builder = proxiima::spiice::DurablePayloadBuilder::new();
        builder.field("value", &self.value)?;
        Ok(builder.finish())
    }
}

#[derive(Debug)]
struct EmbeddedConsumerSpiice;

impl SpiiceBundle for EmbeddedConsumerSpiice {
    fn register(registry: &mut SpiiceRegistry) -> Result<(), SpiiceRegistryError> {
        register_conformance_spiice(registry)
    }

    fn migrators() -> Vec<proxiima::NamedMigrator> {
        Vec::new()
    }
}

#[derive(Debug)]
struct HostedConformanceApp;

impl SpiiceBundle for HostedConformanceApp {
    fn register(registry: &mut SpiiceRegistry) -> Result<(), SpiiceRegistryError> {
        register_conformance_spiice(registry)
    }

    fn migrators() -> Vec<proxiima::NamedMigrator> {
        Vec::new()
    }
}

impl SpiiceApp for HostedConformanceApp {
    fn app_info() -> AppInfo {
        AppInfo {
            id: "proxiima-conformance",
            title: "Proxiima Conformance",
            version: env!("CARGO_PKG_VERSION"),
        }
    }

    fn configure(builder: RuntimeBuilder) -> RuntimeBuilder {
        builder
    }
}

/// The declaration for the spiice below: freeze refuses a spiice that is
/// linked and declares nothing.
const CONFORMANCE_SCHEMA: SchemaContract = SchemaContract {
    id: SchemaRef::new("proxiima-conformance", "fact", 7),
    kind: PayloadKind::Fact,
    sidecar_table: Some("proxiima_conformance.fact_v1"),
    search: SearchProjectionDecl::None {
        why: "a conformance fixture, not a search surface",
        tag_column: None,
    },
    embedding: EmbeddingRecipe::Never {
        why: "a conformance fixture, not a memory",
    },
    transfer: TransferRule::StaysOnKey,
    provenance: Provenance::None,
    surfaces: &[],
    natural_key_columns: proxiima_core::NaturalKeyDeclaration::EMPTY,
};

static CONFORMANCE_CONTRACT: SpiiceContract = SpiiceContract {
    retired_facts: &[],
    spiice_id: "proxiima-conformance",
    // Non-zero: ordinal 0 is core's and cannot be claimed twice.
    ordinal: 7,
    schemas: &[CONFORMANCE_SCHEMA],
    state_surfaces: &[],
    scopes: &[],
    kernel_surfaces: &[],
    tools: &[],
    resources: &[],
    bespoke_erase_legs: &[],
    bespoke_transfer_legs: &[],
    projection: ProjectionDecl::None {
        why: "a conformance fixture registers no search surface",
    },
    evolutions: &[SchemaEvolution::new(
        "proxiima-conformance/fact-v7",
        &[SchemaContract {
            id: SchemaRef::new("proxiima-conformance", "fact", 1),
            ..CONFORMANCE_SCHEMA
        }],
        &[CONFORMANCE_SCHEMA],
        7,
        &[],
        "Conformance fixture declares its historical schema path.",
        "Use the version-seven payload.",
    )],
};

fn register_conformance_spiice(registry: &mut SpiiceRegistry) -> Result<(), SpiiceRegistryError> {
    registry.try_add_spiice(SpiiceDescriptor {
        spiice_id: "proxiima-conformance".to_string(),
        display_name: "Conformance".to_string(),
        package_version: env!("CARGO_PKG_VERSION").to_string(),
        author: None,
        provenance: SpiiceProvenance::Builtin,
    })?;
    registry.try_add_fact_schema::<ConformanceFactV1>()?;
    registry.try_add_contract(&CONFORMANCE_CONTRACT)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RegistryDump {
    schemas: Vec<SchemaDump>,
    sidecar_tables: Vec<String>,
    tool_ids: Vec<String>,
    spiices: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SchemaDump {
    schema_id: String,
    schema_version: u32,
    kind: String,
    sidecar_table: Option<String>,
}

fn frozen_registry_for_bundle<B: SpiiceBundle>() -> SpiiceRegistryFrozen {
    let mut registry = SpiiceRegistry::new();
    B::register(&mut registry).expect("test spiice registration");
    registry.try_freeze().expect("test registry freeze")
}

fn dump_for_embedded_consumer<B: SpiiceBundle>() -> RegistryDump {
    dump_registry(&frozen_registry_for_bundle::<B>())
}

fn dump_for_hosted_app<A: SpiiceApp>() -> RegistryDump {
    // `Proxiima::<A>::build` registers `A`'s bundle before any connection;
    // this helper isolates that static registry composition from DB boot.
    dump_registry(&frozen_registry_for_bundle::<A>())
}

fn dump_registry(registry: &SpiiceRegistryFrozen) -> RegistryDump {
    let mut schemas = registry
        .schemas()
        .iter()
        .map(|schema| SchemaDump {
            schema_id: schema.schema_id.as_str().to_string(),
            schema_version: schema.schema_version.into_inner(),
            kind: format!("{:?}", schema.kind),
            sidecar_table: schema.sidecar_table.clone(),
        })
        .collect::<Vec<_>>();
    schemas.sort();

    let mut sidecar_tables = registry
        .schemas()
        .iter()
        .filter_map(|schema| schema.sidecar_table.clone())
        .collect::<Vec<_>>();
    sidecar_tables.sort();
    sidecar_tables.dedup();

    let mut tool_ids = registry
        .list_mcp_tools()
        .iter()
        .map(|tool| tool.name.to_string())
        .collect::<Vec<_>>();
    tool_ids.sort();

    let mut spiices = registry
        .list_spiices()
        .iter()
        .map(|spiice| spiice.spiice_id.clone())
        .collect::<Vec<_>>();
    spiices.sort();

    RegistryDump {
        schemas,
        sidecar_tables,
        tool_ids,
        spiices,
    }
}

#[test]
fn embedded_and_hosted_registry_dumps_match() {
    let embedded = dump_for_embedded_consumer::<EmbeddedConsumerSpiice>();
    let hosted = dump_for_hosted_app::<HostedConformanceApp>();

    assert_eq!(embedded, hosted);
    assert!(
        embedded
            .spiices
            .contains(&"proxiima-conformance".to_string())
    );
    assert!(embedded.tool_ids.contains(&"core_remember".to_string()));
    assert!(
        embedded
            .sidecar_tables
            .iter()
            .any(|table| table == "proxiima_conformance.fact_v1")
    );
    assert!(embedded.schemas.iter().any(|schema| {
        schema.schema_id == ConformanceFactV1::SCHEMA_ID
            && schema.schema_version == ConformanceFactV1::SCHEMA_VERSION
            && schema.kind == "Fact"
            && schema.sidecar_table.as_deref() == Some("proxiima_conformance.fact_v1")
    }));
}

#[test]
fn registry_dump_is_deterministic() {
    let first = dump_for_embedded_consumer::<EmbeddedConsumerSpiice>();
    let second = dump_for_embedded_consumer::<EmbeddedConsumerSpiice>();

    assert_eq!(first, second);
}

/// Boot, the owner-RLS census and the MCP tool host all take their schema set
/// from this one function, so it names exactly what a registry stores into.
#[test]
fn composed_schema_names_cover_core_and_every_linked_spiice() {
    let mut registry = SpiiceRegistry::new();
    proxiima_example::ExampleSpiice::register(&mut registry).expect("the example spiice registers");
    register_conformance_spiice(&mut registry).expect("the conformance spiice registers");
    let frozen = registry.try_freeze().expect("two spiices freeze");

    assert_eq!(
        proxiima::host::composed_schema_names(&frozen),
        ["proxiima_conformance", "proxiima_core", "proxiima_example"],
        "core's schema plus one per spiice, sorted and without repeats"
    );
    // The facade re-exports the one definition.
    assert_eq!(
        proxiima::host::composed_schema_names(&frozen),
        proxiima_core::composed_schema_names(&frozen)
    );
}

#[test]
fn composed_schema_names_of_a_core_only_registry_is_core() {
    let frozen = SpiiceRegistry::new()
        .try_freeze()
        .expect("a registry with no spiice freezes");

    assert_eq!(
        proxiima::host::composed_schema_names(&frozen),
        ["proxiima_core"]
    );
}
