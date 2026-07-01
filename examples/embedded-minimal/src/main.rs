//! Headless embedded host: one-line facade boot from env, ingest one
//! Fact, query it back. The host wiring template for `FlavorApp`s.

mod flavor;

use proxima::flavor::{FactPayload, SchemaId};
use proxima::{
    CitationSpec, EntityKind, FactWriteCommand, Proxima, QueryRequest, QueryResponse,
    SourceBatchId, UPLOADED_BLOB_SCHEMA_ID,
};

const CORE_CITATION_SCHEMA_ID: &str = "proxima-core/wake-trace-citation-v1";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let booted = Proxima::<flavor::EmbeddedMinimalFlavor>::app()
        .from_env()
        .allow_insecure_single_owner()
        .run()
        .await?;
    let embedding_worker = booted.spawn_embedding_worker(booted.cancel.clone());
    let authz = booted
        .single_owner_authz()
        .expect("insecure single-owner mode is enabled");

    let payload = flavor::DocumentFiledV1 {
        source_path: "/example/intake/r-2026-0001.pdf".into(),
        title: "Example invoice".into(),
    };
    let citation =
        CitationSpec::v1_for_payload(UPLOADED_BLOB_SCHEMA_ID, &payload, CORE_CITATION_SCHEMA_ID);
    let now = time::OffsetDateTime::now_utc();
    let draft = FactWriteCommand::from_payload(
        "embedded-minimal/host",
        SourceBatchId::new(uuid::Uuid::now_v7()),
        &payload,
        now,
    )
    .with_citation(citation);

    let outcome = booted.engine.fact_ingest(&authz, draft).await?;
    println!("ingested: {outcome:?}");

    let response = booted
        .engine
        .query(&authz, &query_for_schema(&booted.owner))
        .await?;
    println!("query returned {} rows", row_count(&response));

    booted.cancel.cancel();
    embedding_worker.await?;
    booted.shutdown().await;
    Ok(())
}

fn query_for_schema(owner: &proxima::Owner) -> QueryRequest {
    let mut req = QueryRequest::for_owner(*owner);
    req.entity_kind = Some(EntityKind::Fact);
    req.schema_id = Some(SchemaId::new(flavor::DocumentFiledV1::SCHEMA_ID.into()));
    req.limit = 10;
    req
}

fn row_count(response: &QueryResponse) -> usize {
    response.memories.len()
}
