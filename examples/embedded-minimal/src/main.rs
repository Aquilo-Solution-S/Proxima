//! Headless embedded host: boot engine from env, ingest one Fact,
//! query it back. The wiring template for real host apps.

mod flavor;

use proxima_core::auth::Credentials;
use proxima_core::verbs::event_ingest::{CitationMappingHint, CitedObjectHint, EventDraft};
use proxima_core::verbs::query::{EntityKind, QueryRequest, QueryResponse};
use proxima_core::{
    FactPayload, SchemaId, SchemaVersion, SourceBatchId, SourceId, UPLOADED_BLOB_SCHEMA_ID,
};
use proxima_embed::{EmbedConfig, ProximaBuilder, company_owner};

const CORE_CITATION_SCHEMA_ID: &str = "proxima-core/wake-trace-citation-v1";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = EmbedConfig::from_env()?;
    let org: uuid::Uuid = std::env::var("PROXIMA_ORG_ID")
        .unwrap_or_else(|_| uuid::Uuid::nil().to_string())
        .parse()?;
    let owner = company_owner(org);

    let booted = ProximaBuilder::new(config, owner.clone())
        .bundle::<flavor::EmbeddedMinimalFlavor>()
        .boot()
        .await?;

    let payload = flavor::DocumentFiledV1 {
        source_path: "/example/intake/r-2026-0001.pdf".into(),
        title: "Example invoice".into(),
    };
    let mut payload_bytes = Vec::new();
    ciborium::ser::into_writer(&payload, &mut payload_bytes)?;
    let content_hash = blake3_hash(&payload_bytes);
    let now = time::OffsetDateTime::now_utc();
    let draft = EventDraft {
        source_id: SourceId::new("embedded-minimal/host"),
        source_batch_id: SourceBatchId::new(uuid::Uuid::now_v7()),
        owner: owner.clone(),
        schema_id: SchemaId::new(flavor::DocumentFiledV1::SCHEMA_ID.into()),
        schema_version: SchemaVersion::new(flavor::DocumentFiledV1::SCHEMA_VERSION),
        payload: payload_bytes,
        observed_at: now,
        occurred_at: now,
        cited_object: CitedObjectHint {
            schema_id: SchemaId::new(UPLOADED_BLOB_SCHEMA_ID.into()),
            schema_version: SchemaVersion::new(1),
            content_hash,
        },
        citation_mapping: CitationMappingHint {
            schema_id: SchemaId::new(CORE_CITATION_SCHEMA_ID.into()),
            schema_version: SchemaVersion::new(1),
        },
    };

    let outcome = booted
        .engine
        .event_ingest(&embedded_credentials(), draft)
        .await?;
    println!("ingested: {outcome:?}");

    let response = booted
        .engine
        .query(&embedded_credentials(), &query_for_schema(&owner))
        .await?;
    println!("query returned {} rows", row_count(&response));

    booted.engine.stop(booted.handle).await;
    Ok(())
}

fn embedded_credentials() -> Credentials {
    Credentials::None
}

fn query_for_schema(owner: &proxima_core::Owner) -> QueryRequest {
    let mut req = QueryRequest::for_owner(owner.clone());
    req.entity_kind = Some(EntityKind::Fact);
    req.schema_id = Some(SchemaId::new(flavor::DocumentFiledV1::SCHEMA_ID.into()));
    req.limit = 10;
    req
}

fn row_count(response: &QueryResponse) -> usize {
    response.memories.len()
}

fn blake3_hash(bytes: &[u8]) -> [u8; 32] {
    *blake3::hash(bytes).as_bytes()
}
