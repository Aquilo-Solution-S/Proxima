use proxima::flavor::{
    FactPayload, FlavorBundle, FlavorDescriptor, FlavorProvenance, FlavorRegistry,
    FlavorRegistryError, FlavorRegistryFrozen, PayloadKeyBuilder,
};
use proxima::{AppInfo, FlavorApp, RuntimeBuilder};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ConformanceFactV1 {
    value: String,
}

impl FactPayload for ConformanceFactV1 {
    const SCHEMA_ID: &'static str = "proxima-conformance/fact-v1";
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
        Some("proxima_conformance.fact_v1")
    }
}

#[derive(Debug)]
struct EmbeddedConsumerFlavor;

impl FlavorBundle for EmbeddedConsumerFlavor {
    fn register(registry: &mut FlavorRegistry) -> Result<(), FlavorRegistryError> {
        register_conformance_flavor(registry)
    }

    fn migrators() -> Vec<proxima::NamedMigrator> {
        Vec::new()
    }
}

#[derive(Debug)]
struct HostedConformanceApp;

impl FlavorBundle for HostedConformanceApp {
    fn register(registry: &mut FlavorRegistry) -> Result<(), FlavorRegistryError> {
        register_conformance_flavor(registry)
    }

    fn migrators() -> Vec<proxima::NamedMigrator> {
        Vec::new()
    }
}

impl FlavorApp for HostedConformanceApp {
    fn app_info() -> AppInfo {
        AppInfo {
            id: "proxima-conformance",
            title: "Proxima Conformance",
            version: env!("CARGO_PKG_VERSION"),
        }
    }

    fn configure(builder: RuntimeBuilder) -> RuntimeBuilder {
        builder
    }
}

fn register_conformance_flavor(registry: &mut FlavorRegistry) -> Result<(), FlavorRegistryError> {
    registry.try_add_flavor(FlavorDescriptor {
        flavor_id: "proxima-conformance".to_string(),
        display_name: "Conformance".to_string(),
        package_version: env!("CARGO_PKG_VERSION").to_string(),
        author: None,
        provenance: FlavorProvenance::Builtin,
    })?;
    registry.try_add_fact_schema::<ConformanceFactV1>()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RegistryDump {
    schemas: Vec<SchemaDump>,
    sidecar_tables: Vec<String>,
    tool_ids: Vec<String>,
    flavors: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SchemaDump {
    schema_id: String,
    schema_version: u32,
    kind: String,
    sidecar_table: Option<String>,
}

fn frozen_registry_for_bundle<B: FlavorBundle>() -> FlavorRegistryFrozen {
    let mut registry = FlavorRegistry::new();
    B::register(&mut registry).expect("test flavor registration");
    registry.try_freeze().expect("test registry freeze")
}

fn dump_for_embedded_consumer<B: FlavorBundle>() -> RegistryDump {
    dump_registry(&frozen_registry_for_bundle::<B>())
}

fn dump_for_hosted_app<A: FlavorApp>() -> RegistryDump {
    // `Proxima::<A>::build` routes through `ProximaBuilder::bundle::<A>()`;
    // this helper isolates that static registry composition from DB boot.
    dump_registry(&frozen_registry_for_bundle::<A>())
}

fn dump_registry(registry: &FlavorRegistryFrozen) -> RegistryDump {
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

    let mut flavors = registry
        .list_flavors()
        .iter()
        .map(|flavor| flavor.flavor_id.clone())
        .collect::<Vec<_>>();
    flavors.sort();

    RegistryDump {
        schemas,
        sidecar_tables,
        tool_ids,
        flavors,
    }
}

#[test]
fn embedded_and_hosted_registry_dumps_match() {
    let embedded = dump_for_embedded_consumer::<EmbeddedConsumerFlavor>();
    let hosted = dump_for_hosted_app::<HostedConformanceApp>();

    assert_eq!(embedded, hosted);
    assert!(
        embedded
            .flavors
            .contains(&"proxima-conformance".to_string())
    );
    assert!(embedded.tool_ids.contains(&"core_remember".to_string()));
    assert!(
        embedded
            .sidecar_tables
            .iter()
            .any(|table| table == "proxima_conformance.fact_v1")
    );
    assert!(embedded.schemas.iter().any(|schema| {
        schema.schema_id == ConformanceFactV1::SCHEMA_ID
            && schema.schema_version == ConformanceFactV1::SCHEMA_VERSION
            && schema.kind == "Fact"
            && schema.sidecar_table.as_deref() == Some("proxima_conformance.fact_v1")
    }));
}

#[test]
fn registry_dump_is_deterministic() {
    let first = dump_for_embedded_consumer::<EmbeddedConsumerFlavor>();
    let second = dump_for_embedded_consumer::<EmbeddedConsumerFlavor>();

    assert_eq!(first, second);
}
