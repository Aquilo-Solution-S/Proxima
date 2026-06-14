//! Headless embedded host: one-line facade boot from env, ingest one
//! Fact, query it back. The host wiring template for `FlavorApp`s.

mod flavor;

use proxima::Proxima;
use proxima_core::verbs::event_ingest::{
    Citation, CitationMappingHint, CitedObjectHint, EventDraft,
};
use proxima_core::verbs::query::{EntityKind, QueryRequest, QueryResponse};
use proxima_core::{
    FactPayload, SchemaId, SchemaVersion, SourceBatchId, SourceId, UPLOADED_BLOB_SCHEMA_ID,
    canonical_json_bytes,
};

const CORE_CITATION_SCHEMA_ID: &str = "proxima-core/wake-trace-citation-v1";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let booted = Proxima::<flavor::EmbeddedMinimalFlavor>::app()
        .from_env()
        .allow_insecure_single_owner()
        .run()
        .await?;
    let authz = booted
        .single_owner_authz()
        .expect("insecure single-owner mode is enabled");

    let payload = flavor::DocumentFiledV1 {
        source_path: "/example/intake/r-2026-0001.pdf".into(),
        title: "Example invoice".into(),
    };
    let payload_value = serde_json::to_value(payload)?;
    let payload_bytes = canonical_json_bytes(&payload_value);
    let content_hash = blake3_hash(&payload_bytes);
    let now = time::OffsetDateTime::now_utc();
    let draft = EventDraft {
        source_id: SourceId::new("embedded-minimal/host"),
        source_batch_id: SourceBatchId::new(uuid::Uuid::now_v7()),
        principal: booted.owner.principal.clone(),
        org_id: Some(booted.owner.org_id),
        author_personality_instance_id: None,
        schema_id: SchemaId::new(flavor::DocumentFiledV1::SCHEMA_ID.into()),
        schema_version: SchemaVersion::new(flavor::DocumentFiledV1::SCHEMA_VERSION),
        payload: payload_bytes,
        rendered_text: None,
        observed_at: now,
        occurred_at: now,
        citation: Some(Citation {
            object: CitedObjectHint {
                schema_id: SchemaId::new(UPLOADED_BLOB_SCHEMA_ID.into()),
                schema_version: SchemaVersion::new(1),
                content_hash,
            },
            mapping: CitationMappingHint {
                schema_id: SchemaId::new(CORE_CITATION_SCHEMA_ID.into()),
                schema_version: SchemaVersion::new(1),
            },
        }),
    };

    let outcome = booted.engine.event_ingest(&authz, draft).await?;
    println!("ingested: {outcome:?}");

    let response = booted
        .engine
        .query(&authz, &query_for_schema(&booted.owner))
        .await?;
    println!("query returned {} rows", row_count(&response));

    booted.shutdown().await;
    Ok(())
}

fn query_for_schema(owner: &proxima_core::Owner) -> QueryRequest {
    let mut req = QueryRequest::for_principal(owner.principal.clone());
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
