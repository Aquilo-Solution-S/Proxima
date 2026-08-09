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
    id = "test/stateful",
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
    id = "test/stateful",
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

#[test]
fn stateful_filters_for_schema_returns_all_versions() {
    let schema_id = SchemaId::new("test/stateful".into());
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

    let filters = registry.stateful_filters_for_schema(&schema_id);
    let versions = filters
        .iter()
        .map(|filter| filter.schema_version.into_inner())
        .collect::<Vec<_>>();
    assert_eq!(versions, vec![1, 2]);
    assert_eq!(
        filters
            .iter()
            .map(|filter| filter.sidecar_table.as_str())
            .collect::<Vec<_>>(),
        vec!["test_schema.stateful_v1", "test_schema.stateful_v2"],
    );
    assert_eq!(
        registry
            .lookup_payload(&schema_id, SchemaVersion::new(1), PayloadKind::Fact)
            .expect("stateful v1 schema")
            .natural_key_columns,
        ["entity_id"]
    );
}
