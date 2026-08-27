use proxima_core::verbs::schema::{PayloadKind, SchemaRequest};
use proxima_core::{
    FactPayload, FactTombstone, FlavorRegistry, PayloadKeyBuilder, SchemaId, SchemaVersion,
};

macro_rules! fact_payload {
    (
        $name:ident,
        id = $schema_id:literal,
        version = $version:literal,
        table = $table:expr,
        natural_key = $natural_key:expr,
        tombstone = $tombstone:expr
    ) => {
        #[derive(Debug, serde::Serialize, serde::Deserialize)]
        struct $name {
            value: String,
        }

        impl FactPayload for $name {
            const SCHEMA_ID: &'static str = $schema_id;
            const SCHEMA_VERSION: u32 = $version;

            fn receipt_key(&self) -> Vec<u8> {
                let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
                key.field_str("value", &self.value);
                key.finish()
            }

            fn render(&self) -> String {
                self.value.clone()
            }

            fn sidecar_table() -> Option<&'static str> {
                $table
            }

            fn natural_key_columns() -> &'static [&'static str] {
                $natural_key
            }

            fn tombstone() -> Option<FactTombstone> {
                $tombstone
            }
        }
    };
}

fact_payload!(
    StatefulV1,
    id = "test/stateful-v1",
    version = 1,
    table = Some("test_schema.stateful_v1"),
    natural_key = &["entity_id"],
    tombstone = Some(FactTombstone {
        column: "state",
        value: "Tombstone",
    })
);
fact_payload!(
    StatefulV2,
    id = "test/stateful-v2",
    version = 2,
    table = Some("test_schema.stateful_v2"),
    natural_key = &["entity_id"],
    tombstone = Some(FactTombstone {
        column: "state",
        value: "Tombstone",
    })
);
fact_payload!(
    StatelessV1,
    id = "test/stateless",
    version = 1,
    table = Some("test_schema.stateless_v1"),
    natural_key = &[],
    tombstone = None
);

#[test]
fn schema_response_lists_the_frozen_registry() {
    let registry = FlavorRegistry::new().freeze_or_panic_for_tests();
    let expected = registry
        .schemas()
        .iter()
        .map(|schema| (schema.schema_id.clone(), schema.schema_version, schema.kind))
        .collect::<Vec<_>>();

    let actual = registry
        .handle(&SchemaRequest)
        .schemas
        .into_iter()
        .map(|schema| (schema.schema_id, schema.schema_version, schema.kind))
        .collect::<Vec<_>>();

    assert_eq!(actual, expected);
}

/// Every registered stateful Fact selector carries its own sidecar table and
/// natural-key columns. Memory stores no schema version, so a new version uses
/// a new selector rather than creating an ambiguous `(Fact, schema_id)` pair.
#[test]
fn every_stateful_fact_selector_registers_its_natural_key() {
    let v1_schema_id = SchemaId::new("test/stateful-v1".into());
    let v2_schema_id = SchemaId::new("test/stateful-v2".into());
    let mut registry = FlavorRegistry::new();
    registry
        .try_add_fact_schema::<StatefulV1>()
        .expect("stateful v1 registration");
    registry
        .try_add_fact_schema::<StatefulV2>()
        .expect("stateful v2 registration");
    registry
        .try_add_fact_schema::<StatelessV1>()
        .expect("stateless registration");
    let registry = registry.try_freeze().expect("typed test schemas freeze");

    let stateful = registry
        .list()
        .into_iter()
        .filter(|info| {
            (info.schema_id == v1_schema_id || info.schema_id == v2_schema_id)
                && info.kind == PayloadKind::Fact
        })
        .collect::<Vec<_>>();
    let versions = stateful
        .iter()
        .map(|info| info.schema_version.into_inner())
        .collect::<Vec<_>>();
    assert_eq!(versions, vec![1, 2]);
    assert_eq!(
        stateful
            .iter()
            .map(|info| info.sidecar_table.as_deref().expect("sidecar table"))
            .collect::<Vec<_>>(),
        vec!["test_schema.stateful_v1", "test_schema.stateful_v2"],
    );
    assert!(
        stateful
            .iter()
            .all(|info| !info.natural_key_columns.is_empty()),
        "every stateful version must declare its natural key"
    );
    assert_eq!(
        registry
            .lookup_payload(&v1_schema_id, SchemaVersion::new(1), PayloadKind::Fact,)
            .expect("stateful v1 schema")
            .natural_key_columns,
        ["entity_id"]
    );
    assert_eq!(
        registry
            .lookup_payload(&v2_schema_id, SchemaVersion::new(2), PayloadKind::Fact,)
            .expect("stateful v2 schema")
            .natural_key_columns,
        ["entity_id"]
    );
}
