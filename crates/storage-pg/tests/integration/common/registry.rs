use proxima_core::verbs::schema::PayloadKind;
use proxima_core::{
    FactPayload, FlavorRegistry, FlavorRegistryFrozen, GoalPayload, PayloadKeyBuilder, SchemaId,
    SchemaVersion,
};

macro_rules! stateless_fact_payload {
    ($name:ident, $schema_id:literal, $version:literal) => {
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
        }
    };
}

macro_rules! goal_payload {
    ($name:ident, $schema_id:literal, $version:literal) => {
        #[derive(Debug, serde::Serialize, serde::Deserialize)]
        struct $name {
            value: String,
        }

        impl GoalPayload for $name {
            const SCHEMA_ID: &'static str = $schema_id;
            const SCHEMA_VERSION: u32 = $version;

            fn goal_key(&self) -> Vec<u8> {
                let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
                key.field_str("value", &self.value);
                key.finish()
            }
        }
    };
}

stateless_fact_payload!(FactBlobV1, "test/fact_blob", 1);
stateless_fact_payload!(FactBlobV2, "test/fact_blob_v2", 2);
stateless_fact_payload!(ComplianceFactV1, "test/compliance_fact", 1);
stateless_fact_payload!(EmbeddingLifecycleFactV1, "test/embedding-lifecycle-fact", 1);
stateless_fact_payload!(ReceiptlessFactV1, "test/receiptless_fact", 1);
stateless_fact_payload!(SidecarFactV1, "test/sidecar_fact", 1);
goal_payload!(GoalBlobV1, "test/goal_blob", 1);
goal_payload!(GoalBlobV2, "test/goal_blob_v2", 2);

const CITED_BLOB_SCHEMA_ID: &str = "test/cited_blob";
const CITATION_BLOB_SCHEMA_ID: &str = "test/citation_blob";
const SIDECAR_CITED_SCHEMA_ID: &str = "test/sidecar_cited";
const SIDECAR_CITATION_SCHEMA_ID: &str = "test/sidecar_citation";

fn register_fact<P: FactPayload>(registry: &mut FlavorRegistry) {
    registry
        .try_add_fact_schema::<P>()
        .expect("typed test Fact registration");
}

fn register_opaque_citation_pair(
    registry: &mut FlavorRegistry,
    cited_object_schema_id: &str,
    citation_mapping_schema_id: &str,
) {
    registry
        .try_add_opaque_schema(
            SchemaId::new(cited_object_schema_id.into()),
            SchemaVersion::new(1),
            PayloadKind::CitedObject,
        )
        .expect("opaque test cited-object registration");
    registry
        .try_add_opaque_schema(
            SchemaId::new(citation_mapping_schema_id.into()),
            SchemaVersion::new(1),
            PayloadKind::CitationMapping,
        )
        .expect("opaque test citation-mapping registration");
}

fn freeze_with_fact<P: FactPayload>() -> FlavorRegistryFrozen {
    let mut registry = FlavorRegistry::new();
    register_fact::<P>(&mut registry);
    registry.freeze_or_panic_for_tests()
}

pub fn fact_blob_registry() -> FlavorRegistryFrozen {
    let mut registry = FlavorRegistry::new();
    register_fact::<FactBlobV1>(&mut registry);
    register_opaque_citation_pair(&mut registry, CITED_BLOB_SCHEMA_ID, CITATION_BLOB_SCHEMA_ID);
    registry.freeze_or_panic_for_tests()
}

pub fn fact_blob_only_registry() -> FlavorRegistryFrozen {
    freeze_with_fact::<FactBlobV1>()
}

pub fn compliance_fact_registry() -> FlavorRegistryFrozen {
    freeze_with_fact::<ComplianceFactV1>()
}

pub fn embedding_lifecycle_registry() -> FlavorRegistryFrozen {
    freeze_with_fact::<EmbeddingLifecycleFactV1>()
}

pub fn receiptless_fact_registry() -> FlavorRegistryFrozen {
    freeze_with_fact::<ReceiptlessFactV1>()
}

pub fn sidecar_fact_registry() -> FlavorRegistryFrozen {
    let mut registry = FlavorRegistry::new();
    register_fact::<SidecarFactV1>(&mut registry);
    register_opaque_citation_pair(
        &mut registry,
        SIDECAR_CITED_SCHEMA_ID,
        SIDECAR_CITATION_SCHEMA_ID,
    );
    registry.freeze_or_panic_for_tests()
}

pub fn query_registry() -> FlavorRegistryFrozen {
    let mut registry = FlavorRegistry::new();
    register_fact::<FactBlobV1>(&mut registry);
    register_fact::<FactBlobV2>(&mut registry);
    registry
        .try_add_goal_schema::<GoalBlobV1>()
        .expect("typed test Goal v1 registration");
    registry
        .try_add_goal_schema::<GoalBlobV2>()
        .expect("typed test Goal v2 registration");
    register_opaque_citation_pair(&mut registry, CITED_BLOB_SCHEMA_ID, CITATION_BLOB_SCHEMA_ID);
    registry.freeze_or_panic_for_tests()
}
