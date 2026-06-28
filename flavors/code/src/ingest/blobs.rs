use proxima_core::verbs::event_ingest::{CitationSpec, EventIngestOutcome};
use proxima_core::{
    AbstractionPayload, CORE_DERIVED_FROM_RELATION, EdgeAuthorshipKind, EntityKind, FactPayload,
    MemoryId, MemoryOperatorKind, Owner, SchemaVersion, SourceBatchId,
};
use proxima_storage_pg::sidecars::PgMemorySidecar;
use proxima_storage_pg::verbs::derive_append::{
    DerivedDraft, DerivedOutcome, append_derived_in_tx,
};
use proxima_storage_pg::verbs::edge_append::{Endpoint, append_edge};
use proxima_storage_pg::verbs::event_ingest::{FactIngestContext, ingest_fact_with_sidecar};
use sqlx::PgPool;

use crate::payloads::{CodeChunkV1, CommitV1, FileRevisionV1};

use super::IngestError;
use super::schemas::{
    CODE_BLOB_SCHEMA, CODE_BLOB_WHOLE_SCHEMA, CODE_COMMIT_OBJECT_SCHEMA, CODE_COMMIT_WHOLE_SCHEMA,
    LOCAL_GIT_SOURCE_ID, schema_registry,
};

const CODE_SLICE_OPERATOR_MODEL: &str = "proxima-code/local-git-source";
const CODE_SLICE_PROMPT_VERSION: &str = "code-slice-v1";

const CODE_SLICE_NAMESPACE: uuid::Uuid = uuid::Uuid::from_bytes([
    0x8d, 0xb6, 0x89, 0x67, 0x17, 0x34, 0x44, 0x11, 0xaa, 0xe6, 0x68, 0xef, 0x6c, 0x2a, 0x31, 0x8d,
]);

const CODE_SLICE_PROVENANCE_EDGE_NAMESPACE: uuid::Uuid = uuid::Uuid::from_bytes([
    0x59, 0x1f, 0x17, 0x5c, 0x76, 0x04, 0x46, 0x46, 0x9d, 0x17, 0x75, 0x9e, 0x87, 0xf0, 0xe0, 0x7a,
]);

/// Close a `source_batch` opened by the typed-ingest helpers under a
/// LocalGitSource poll. Idempotent. Maps `NotFound` to `Ok(())` so
/// callers can safely call this after no-op polls (no events → no
/// batch row was ever inserted).
pub async fn close_local_git_batch(
    pool: &PgPool,
    owner: &Owner,
    source_batch_id: SourceBatchId,
) -> Result<(), IngestError> {
    match proxima_storage_pg::verbs::close_batch::close_batch(pool, owner, source_batch_id).await {
        Ok(_) | Err(proxima_core::StorageError::NotFound) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

fn local_git_context(
    owner: &Owner,
    source_batch_id: SourceBatchId,
    observed_at: time::OffsetDateTime,
) -> FactIngestContext<'_> {
    FactIngestContext::new(owner, LOCAL_GIT_SOURCE_ID, source_batch_id).observed_at(observed_at)
}

async fn ingest_local_git_fact<P>(
    pool: &PgPool,
    owner: &Owner,
    source_batch_id: SourceBatchId,
    payload: &P,
    citation: CitationSpec,
    observed_at: time::OffsetDateTime,
) -> Result<EventIngestOutcome, IngestError>
where
    P: FactPayload + PgMemorySidecar + Clone,
{
    let ctx = local_git_context(owner, source_batch_id, observed_at);
    let mut tx = pool.begin().await?;
    let outcome = ingest_fact_with_sidecar(&mut tx, &ctx, payload, citation).await?;
    tx.commit().await?;
    Ok(outcome)
}

/// Atomic Fact + sidecar write for `commit-v1`. Cites the commit
/// object (keyed by blake3 of the commit sha) with a "whole-commit"
/// CitationMapping.
pub async fn ingest_commit(
    pool: &PgPool,
    owner: &Owner,
    source_batch_id: SourceBatchId,
    payload: &CommitV1,
    observed_at: time::OffsetDateTime,
) -> Result<EventIngestOutcome, IngestError> {
    ingest_local_git_fact(
        pool,
        owner,
        source_batch_id,
        payload,
        CitationSpec::v1(
            CODE_COMMIT_OBJECT_SCHEMA,
            blake3::hash(payload.sha.as_bytes()).into(),
            CODE_COMMIT_WHOLE_SCHEMA,
        ),
        observed_at,
    )
    .await
}

/// Atomic Fact + sidecar write for `file-revision-v1`. Cites the
/// file blob (keyed by `content_sha256`) with a "whole-blob"
/// CitationMapping. Tombstones cite the null blob (`[0u8; 32]`).
pub async fn ingest_file_revision(
    pool: &PgPool,
    owner: &Owner,
    source_batch_id: SourceBatchId,
    payload: &FileRevisionV1,
    observed_at: time::OffsetDateTime,
) -> Result<EventIngestOutcome, IngestError> {
    ingest_local_git_fact(
        pool,
        owner,
        source_batch_id,
        payload,
        CitationSpec::v1(
            CODE_BLOB_SCHEMA,
            payload.content_sha256,
            CODE_BLOB_WHOLE_SCHEMA,
        ),
        observed_at,
    )
    .await
}

/// Atomic derived code-slice Abstraction + sidecar write plus
/// `core/derived-from` provenance edges to the source file-revision
/// Fact and, when available, the source commit Fact.
///
/// Chunking is deterministic F→A operator work over file/blob Facts;
/// this helper deliberately does not write an event, source batch, or
/// Fact citation.
pub async fn append_code_slice(
    pool: &PgPool,
    owner: &Owner,
    payload: &CodeChunkV1,
    source_file_revision: MemoryId,
    source_commit: Option<MemoryId>,
) -> Result<DerivedOutcome, IngestError> {
    let memory_id = code_slice_memory_id(payload, source_file_revision);
    let mut tx = pool.begin().await?;
    let text = render_code_slice(payload);
    let draft = DerivedDraft {
        memory_id,
        owner: *owner,
        kind: EntityKind::Abstraction,
        author_personality_instance_id: None,
        schema_id: <CodeChunkV1 as AbstractionPayload>::schema_id(),
        schema_version: SchemaVersion::new(CodeChunkV1::SCHEMA_VERSION),
        text,
        operator_kind: MemoryOperatorKind::FtoA,
        model_id: CODE_SLICE_OPERATOR_MODEL,
        prompt_version: CODE_SLICE_PROMPT_VERSION,
        supersedes: None,
        embedding: None,
        embedding_model_id: None,
    };
    let sidecar_payload = payload.clone();
    let outcome = append_derived_in_tx(&mut tx, &draft, move |tx, outcome| {
        Box::pin(async move {
            sidecar_payload
                .insert_memory_sidecar(tx, outcome.memory_id)
                .await
        })
    })
    .await?;
    if !outcome.idempotent_replay {
        append_code_slice_provenance(&mut tx, owner, outcome.memory_id, source_file_revision)
            .await?;
        if let Some(commit) = source_commit {
            append_code_slice_provenance(&mut tx, owner, outcome.memory_id, commit).await?;
        }
    }
    tx.commit().await?;
    Ok(outcome)
}

fn code_slice_memory_id(payload: &CodeChunkV1, source_file_revision: MemoryId) -> uuid::Uuid {
    let mut key = Vec::with_capacity(96 + payload.file_path.len());
    key.extend_from_slice(CODE_SLICE_PROMPT_VERSION.as_bytes());
    key.push(0);
    key.extend_from_slice(source_file_revision.into_inner().as_bytes());
    key.push(0);
    key.extend_from_slice(payload.repo_id.as_bytes());
    key.push(0);
    key.extend_from_slice(payload.file_path.as_bytes());
    key.push(0);
    key.extend_from_slice(&payload.chunk_index.to_be_bytes());
    key.push(0);
    key.extend_from_slice(payload.state_marker().as_bytes());
    uuid::Uuid::new_v5(&CODE_SLICE_NAMESPACE, &key)
}

async fn append_code_slice_provenance(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    owner: &Owner,
    code_slice: MemoryId,
    source_fact: MemoryId,
) -> Result<(), IngestError> {
    let registry = schema_registry();
    let relation = registry
        .resolve_relation(CORE_DERIVED_FROM_RELATION)
        .ok_or_else(|| {
            IngestError::Storage(format!(
                "missing registered relation {CORE_DERIVED_FROM_RELATION}"
            ))
        })?;
    let edge_id = derived_from_edge_id(code_slice, source_fact);
    append_edge(
        tx.as_mut(),
        edge_id,
        relation,
        Endpoint::abstraction(code_slice),
        Endpoint::fact(source_fact),
        EdgeAuthorshipKind::OperatorFtoA,
        Some(code_slice),
        owner,
    )
    .await?;
    Ok(())
}

fn derived_from_edge_id(code_slice: MemoryId, source_fact: MemoryId) -> uuid::Uuid {
    let mut key = Vec::with_capacity(80);
    key.extend_from_slice(code_slice.into_inner().as_bytes());
    key.push(0);
    key.extend_from_slice(CORE_DERIVED_FROM_RELATION.as_bytes());
    key.push(0);
    key.extend_from_slice(source_fact.into_inner().as_bytes());
    uuid::Uuid::new_v5(&CODE_SLICE_PROVENANCE_EDGE_NAMESPACE, &key)
}

fn render_code_slice(payload: &CodeChunkV1) -> String {
    match payload.state {
        crate::payloads::FileState::Present => format!(
            "{}:{}-{}",
            payload.file_path, payload.line_range_start, payload.line_range_end
        ),
        crate::payloads::FileState::Tombstone => {
            format!(
                "(deleted slice) {}#{}",
                payload.file_path, payload.chunk_index
            )
        }
    }
}

trait CodeSliceStateMarker {
    fn state_marker(&self) -> &'static str;
}

impl CodeSliceStateMarker for CodeChunkV1 {
    fn state_marker(&self) -> &'static str {
        match self.state {
            crate::payloads::FileState::Present => "Present",
            crate::payloads::FileState::Tombstone => "Tombstone",
        }
    }
}
