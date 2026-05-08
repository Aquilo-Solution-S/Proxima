//! Test fixtures for personality wake / dispatcher behavior tests.
//!
//! Each integration-test binary that exercises the substrate dispatcher
//! `mod common; use common::personality::*;` to pick these up.

#![allow(dead_code, clippy::doc_markdown, clippy::unnecessary_literal_bound)]

use std::sync::Arc;

use async_trait::async_trait;
use proxima_core::auth::NoAuth;
use proxima_core::engine::Engine;
use proxima_core::llm::{AnthropicClient, EmbeddingClient, LlmError};
use proxima_core::personality::InstantiatePersonalityResponse;
use proxima_core::verbs::event_ingest::{CitationMappingHint, CitedObjectHint, EventDraft};
use proxima_core::verbs::query::MemoryStore;
use proxima_core::{
    AbstractionPayload, FlavorDescriptor, FlavorProvenance, FlavorRegistry,
    InstantiatePersonalityRequest, Owner, PerspectivePayload, ProtocolError, SchemaId,
    SchemaVersion, SourceBatchId, SourceId,
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

    fn render(&self) -> String {
        self.label.clone()
    }

    fn sidecar_table() -> &'static str {
        "proxima_test.test_fact_v1"
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestOtherFactV1 {
    pub label: String,
}

impl FactPayload for TestOtherFactV1 {
    const SCHEMA_ID: &'static str = TEST_OTHER_FACT_SCHEMA;
    const SCHEMA_VERSION: u32 = 1;

    fn render(&self) -> String {
        self.label.clone()
    }

    fn sidecar_table() -> &'static str {
        "proxima_test.test_other_fact_v1"
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

/// Trivial embedding client that returns a fixed-length zero vector.
/// Tests that exercise emit_abstraction / emit_perspective need an
/// embedding client wired but don't care about real vectors.
#[derive(Debug)]
pub struct FakeEmbedding {
    pub dim: usize,
}

#[async_trait]
impl EmbeddingClient for FakeEmbedding {
    async fn embed(&self, _text: &str) -> Result<Vec<f32>, LlmError> {
        Ok(vec![0.0; self.dim])
    }

    fn model_id(&self) -> &str {
        "fake-embed"
    }

    fn dim(&self) -> usize {
        self.dim
    }
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
    use proxima_core::Principal;

    let owner = super::owner_fixture();
    let mut registry = FlavorRegistry::new();
    registry.add_flavor(test_flavor_descriptor());
    registry.add_fact_schema::<TestFactV1>();
    registry.add_fact_schema::<TestOtherFactV1>();
    registry.add_perspective_schema::<TestPerspectiveV1>();
    registry.add_perspective_schema::<TestPersonalitySelfV1>();
    registry.add_abstraction_schema::<TestAbstractionV1>();
    let frozen = registry.freeze();
    let principal: Principal = owner.principal.clone();
    Engine::new(
        frozen,
        MemoryStore::new(),
        Box::new(NoAuth::new(principal, owner)),
    )
    .with_storage(Arc::new(pg))
    .with_anthropic(anthropic)
    .with_embed(Arc::new(FakeEmbedding { dim: 8 }))
}

/// Instantiate the test personality + return its instance id.
pub async fn instantiate_test_personality(
    engine: &Engine,
    owner: &Owner,
) -> Result<InstantiatePersonalityResponse, ProtocolError> {
    engine
        .instantiate_personality(InstantiatePersonalityRequest {
            owner: owner.clone(),
            display_name: "Test Personality".into(),
            purpose: "test".into(),
        })
        .await
}

/// Ingest one matching fact via the standard event-ingest verb. Returns
/// the resulting memory_id.
pub async fn ingest_test_fact(pg: &PgStorage, owner: &Owner, label: &str) -> MemoryId {
    use proxima_core::Storage;
    let now = time::OffsetDateTime::now_utc();
    let payload = serde_json::to_vec(&TestFactV1 {
        label: label.into(),
    })
    .expect("serializes");
    let draft = EventDraft {
        source_id: SourceId::new(TEST_SOURCE_ID),
        source_batch_id: SourceBatchId::new(Uuid::now_v7()),
        owner: owner.clone(),
        schema_id: SchemaId::new(TEST_FACT_SCHEMA.into()),
        schema_version: SchemaVersion::new(1),
        payload,
        observed_at: now,
        occurred_at: now,
        cited_object: CitedObjectHint {
            schema_id: SchemaId::new("proxima-test/cited-v1".into()),
            schema_version: SchemaVersion::new(1),
            content_hash: rand_content_hash(),
        },
        citation_mapping: CitationMappingHint {
            schema_id: SchemaId::new("proxima-test/citation-v1".into()),
            schema_version: SchemaVersion::new(1),
        },
    };
    let outcome = pg
        .ingest_event_atomic(&draft)
        .await
        .expect("ingest_event_atomic");
    outcome.memory_id
}

/// Same as `ingest_test_fact` but for the `TEST_OTHER_FACT_SCHEMA` —
/// useful when you want events that don't match the test personality's
/// wake filter.
pub async fn ingest_other_fact(pg: &PgStorage, owner: &Owner, label: &str) -> MemoryId {
    use proxima_core::Storage;
    let now = time::OffsetDateTime::now_utc();
    let payload = serde_json::to_vec(&TestOtherFactV1 {
        label: label.into(),
    })
    .expect("serializes");
    let draft = EventDraft {
        source_id: SourceId::new(TEST_SOURCE_ID),
        source_batch_id: SourceBatchId::new(Uuid::now_v7()),
        owner: owner.clone(),
        schema_id: SchemaId::new(TEST_OTHER_FACT_SCHEMA.into()),
        schema_version: SchemaVersion::new(1),
        payload,
        observed_at: now,
        occurred_at: now,
        cited_object: CitedObjectHint {
            schema_id: SchemaId::new("proxima-test/cited-v1".into()),
            schema_version: SchemaVersion::new(1),
            content_hash: rand_content_hash(),
        },
        citation_mapping: CitationMappingHint {
            schema_id: SchemaId::new("proxima-test/citation-v1".into()),
            schema_version: SchemaVersion::new(1),
        },
    };
    let outcome = pg
        .ingest_event_atomic(&draft)
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
