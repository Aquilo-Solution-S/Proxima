//! Test fixtures for personality wake / dispatcher behavior tests.
//!
//! Each integration-test binary that exercises the substrate dispatcher
//! `mod common; use common::personality::*;` to pick these up.

#![allow(dead_code, clippy::doc_markdown, clippy::unnecessary_literal_bound)]

use proxima_core::FactIngestPort;

use std::sync::Arc;

use proxima_core::engine::Engine;
use proxima_core::llm::AnthropicClient;
use proxima_core::personality::InstantiatePersonalityResponse;
use proxima_core::test_fixtures::ConstantEmbedding;
use proxima_core::verbs::event_ingest::{
    Citation, CitationMappingHint, CitedObjectHint, EventDraft,
};
use proxima_core::{
    AbstractionPayload, AuthPath, AuthzContext, FlavorDescriptor, FlavorProvenance, FlavorRegistry,
    InstantiatePersonalityRequest, Owner, PayloadKeyBuilder, PerspectivePayload, ProtocolError,
    SchemaId, SchemaVersion, SourceBatchId, SourceId,
};
use proxima_core::{FactPayload, MemoryId};
use proxima_storage_pg::PgStorage;
use serde::{Deserialize, Serialize};
use sqlx::Executor;
use uuid::Uuid;

pub const TEST_SOURCE_ID: &str = "proxima-test/source";
pub const TEST_FACT_SCHEMA: &str = "proxima-test/test-fact-v1";
pub const TEST_OTHER_FACT_SCHEMA: &str = "proxima-test/test-other-fact-v1";
pub const TEST_PERSPECTIVE_SCHEMA: &str = "proxima-test/test-perspective-v1";
pub const TEST_ABSTRACTION_SCHEMA: &str = "proxima-test/test-abstraction-v1";
pub const TEST_PERSONALITY_SELF_SCHEMA: &str = "proxima-test/test-personality-self-v1";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestFactV1 {
    pub label: String,
}

impl FactPayload for TestFactV1 {
    const SCHEMA_ID: &'static str = TEST_FACT_SCHEMA;
    const SCHEMA_VERSION: u32 = 1;

    fn event_key(&self) -> Vec<u8> {
        let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
        key.field_str("label", &self.label);
        key.finish()
    }

    fn render(&self) -> String {
        self.label.clone()
    }

    fn sidecar_table() -> Option<&'static str> {
        Some("proxima_test.test_fact_v1")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestOtherFactV1 {
    pub label: String,
}

impl FactPayload for TestOtherFactV1 {
    const SCHEMA_ID: &'static str = TEST_OTHER_FACT_SCHEMA;
    const SCHEMA_VERSION: u32 = 1;

    fn event_key(&self) -> Vec<u8> {
        let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
        key.field_str("label", &self.label);
        key.finish()
    }

    fn render(&self) -> String {
        self.label.clone()
    }

    fn sidecar_table() -> Option<&'static str> {
        Some("proxima_test.test_other_fact_v1")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestAbstractionV1 {
    pub label: String,
}

impl AbstractionPayload for TestAbstractionV1 {
    const SCHEMA_ID: &'static str = TEST_ABSTRACTION_SCHEMA;
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_test.test_abstraction_v1"
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestPerspectiveV1 {
    pub label: String,
}

impl PerspectivePayload for TestPerspectiveV1 {
    const SCHEMA_ID: &'static str = TEST_PERSPECTIVE_SCHEMA;
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_test.test_perspective_v1"
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestPersonalitySelfV1 {
    pub display_name: String,
    pub purpose: String,
}

impl PerspectivePayload for TestPersonalitySelfV1 {
    const SCHEMA_ID: &'static str = TEST_PERSONALITY_SELF_SCHEMA;
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_test.test_personality_self_v1"
    }
}

/// Apply the `proxima_test` schema and sidecar tables required by the
/// test fixture payloads above. Idempotent — uses `IF NOT EXISTS`.
pub async fn apply_test_schemas(pool: &sqlx::PgPool) -> sqlx::Result<()> {
    pool.execute(
        "CREATE SCHEMA IF NOT EXISTS proxima_test; \
         CREATE TABLE IF NOT EXISTS proxima_test.test_fact_v1 ( \
             memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id), \
             label text NOT NULL \
         ); \
         CREATE TABLE IF NOT EXISTS proxima_test.test_other_fact_v1 ( \
             memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id), \
             label text NOT NULL \
         ); \
         CREATE TABLE IF NOT EXISTS proxima_test.test_abstraction_v1 ( \
             memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id), \
             label text NOT NULL \
         ); \
         CREATE TABLE IF NOT EXISTS proxima_test.test_perspective_v1 ( \
             memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id), \
             label text NOT NULL \
         ); \
         CREATE TABLE IF NOT EXISTS proxima_test.test_personality_self_v1 ( \
             memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id), \
             display_name text NOT NULL, \
             purpose text NOT NULL \
         );",
    )
    .await
    .map(|_| ())
}

/// Build an `Engine` over the given storage pool wired with all test
/// schemas + an injected scripted `FlavorDescriptor` for the
/// `proxima-test` prefix that all test payloads use.
#[must_use]
pub fn test_flavor_descriptor() -> FlavorDescriptor {
    FlavorDescriptor {
        flavor_id: "proxima-test".to_string(),
        display_name: "Proxima Test".to_string(),
        package_version: "0.0.0".to_string(),
        author: None,
        provenance: FlavorProvenance::Builtin,
    }
}

/// Anthropic client and a fake embedding client.
#[must_use]
pub fn build_test_engine(pg: PgStorage, anthropic: Arc<dyn AnthropicClient>) -> Engine {
    let mut registry = FlavorRegistry::new();
    registry.add_flavor(test_flavor_descriptor());
    registry.add_fact_schema::<TestFactV1>();
    registry.add_fact_schema::<TestOtherFactV1>();
    registry.add_perspective_schema::<TestPerspectiveV1>();
    registry.add_perspective_schema::<TestPersonalitySelfV1>();
    registry.add_abstraction_schema::<TestAbstractionV1>();
    let frozen = registry.freeze();
    Engine::new(frozen)
        .with_storage_ports(Arc::new(pg).storage_ports())
        .with_anthropic(anthropic)
        .with_embed(Arc::new(ConstantEmbedding::zero("fake-embed")))
}

/// Instantiate the test personality + return its instance id.
pub async fn instantiate_test_personality(
    engine: &Engine,
    owner: &Owner,
) -> Result<InstantiatePersonalityResponse, ProtocolError> {
    engine
        .instantiate_personality(
            &AuthzContext::single_owner(owner, AuthPath::System),
            InstantiatePersonalityRequest {
                principal: *owner,
                display_name: "Test Personality".into(),
            },
        )
        .await
}

/// Ingest one matching fact via the standard event-ingest verb. Returns
/// the resulting memory_id.
pub async fn ingest_test_fact(pg: &PgStorage, owner: &Owner, label: &str) -> MemoryId {
    let now = time::OffsetDateTime::now_utc();
    let payload = serde_json::to_vec(&TestFactV1 {
        label: label.into(),
    })
    .expect("serializes");
    let draft = EventDraft {
        source_id: SourceId::new(TEST_SOURCE_ID),
        source_batch_id: SourceBatchId::new(Uuid::now_v7()),
        principal: *owner,
        author_personality_instance_id: None,
        schema_id: SchemaId::new(TEST_FACT_SCHEMA.into()),
        schema_version: SchemaVersion::new(1),
        payload,
        rendered_text: None,
        observed_at: now,
        occurred_at: now,
        citation: Some(Citation {
            object: CitedObjectHint {
                schema_id: SchemaId::new("proxima-test/cited-v1".into()),
                schema_version: SchemaVersion::new(1),
                content_hash: rand_content_hash(),
            },
            mapping: CitationMappingHint {
                schema_id: SchemaId::new("proxima-test/citation-v1".into()),
                schema_version: SchemaVersion::new(1),
            },
        }),
    };
    let outcome = pg
        .ingest_event_atomic(&draft, None)
        .await
        .expect("ingest_event_atomic");
    outcome.memory_id
}

/// Same as `ingest_test_fact` but for the `TEST_OTHER_FACT_SCHEMA` —
/// useful when you want events that don't match the test personality's
/// wake filter.
pub async fn ingest_other_fact(pg: &PgStorage, owner: &Owner, label: &str) -> MemoryId {
    let now = time::OffsetDateTime::now_utc();
    let payload = serde_json::to_vec(&TestOtherFactV1 {
        label: label.into(),
    })
    .expect("serializes");
    let draft = EventDraft {
        source_id: SourceId::new(TEST_SOURCE_ID),
        source_batch_id: SourceBatchId::new(Uuid::now_v7()),
        principal: *owner,
        author_personality_instance_id: None,
        schema_id: SchemaId::new(TEST_OTHER_FACT_SCHEMA.into()),
        schema_version: SchemaVersion::new(1),
        payload,
        rendered_text: None,
        observed_at: now,
        occurred_at: now,
        citation: Some(Citation {
            object: CitedObjectHint {
                schema_id: SchemaId::new("proxima-test/cited-v1".into()),
                schema_version: SchemaVersion::new(1),
                content_hash: rand_content_hash(),
            },
            mapping: CitationMappingHint {
                schema_id: SchemaId::new("proxima-test/citation-v1".into()),
                schema_version: SchemaVersion::new(1),
            },
        }),
    };
    let outcome = pg
        .ingest_event_atomic(&draft, None)
        .await
        .expect("ingest_event_atomic");
    outcome.memory_id
}

fn rand_content_hash() -> [u8; 32] {
    let mut out = [0u8; 32];
    for (i, byte) in Uuid::now_v7()
        .as_bytes()
        .iter()
        .chain(Uuid::now_v7().as_bytes().iter())
        .take(32)
        .enumerate()
    {
        out[i] = *byte;
    }
    out
}
