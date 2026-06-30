use proxima_core::verbs::fact_ingest::{CitationSpec, FactIngestOutcome};
use proxima_core::{
    AbstractionPayload, CORE_DERIVED_FROM_RELATION, DerivedEdgeSpec, EdgeAuthorshipKind,
    EntityKind, FactPayload, InputContractId, MemoryId, MemoryOperatorKind, OperatorId, Owner,
    RegisteredRelation, SchemaVersion, SourceBatchId,
};
use proxima_storage_pg::sidecars::PgMemorySidecar;
use proxima_storage_pg::verbs::derive_append::{
    DerivedDraft, DerivedOutcome, append_derived_with_edges_in_tx,
};
use proxima_storage_pg::verbs::fact_ingest::{FactIngestContext, ingest_fact_with_sidecar};
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

const CODE_SLICE_OPERATOR_NAMESPACE: uuid::Uuid = uuid::Uuid::from_bytes([
    0x11, 0x88, 0x62, 0x9f, 0xc8, 0xd5, 0x4d, 0x6f, 0x8d, 0x15, 0x91, 0x45, 0xe2, 0x50, 0x43, 0x18,
]);

const CODE_SLICE_INPUT_CONTRACT_NAMESPACE: uuid::Uuid = uuid::Uuid::from_bytes([
    0x2d, 0xee, 0xa3, 0x7f, 0x83, 0xb5, 0x43, 0x7e, 0x93, 0x95, 0x5b, 0xad, 0x1e, 0x41, 0xb0, 0x33,
]);

fn code_slice_operator_id() -> OperatorId {
    OperatorId::new(uuid::Uuid::new_v5(
        &CODE_SLICE_OPERATOR_NAMESPACE,
        b"proxima-code/local-git-source:code-slice-v1",
    ))
}

fn code_slice_input_contract_id(
    payload: &CodeChunkV1,
    source_file_revision: MemoryId,
) -> InputContractId {
    let mut key = Vec::with_capacity(128 + payload.file_path.len());
    key.extend_from_slice(b"proxima-code/code-slice:local-git-file-facts-v1");
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
    InputContractId::new(uuid::Uuid::new_v5(
        &CODE_SLICE_INPUT_CONTRACT_NAMESPACE,
        &key,
    ))
}

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
) -> Result<FactIngestOutcome, IngestError>
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
) -> Result<FactIngestOutcome, IngestError> {
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
) -> Result<FactIngestOutcome, IngestError> {
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
/// callers must materialize all batch Facts and close `source_batch_id`
/// before invoking this helper. This helper deliberately does not write
/// an event, source batch, or Fact citation.
pub async fn append_code_slice(
    pool: &PgPool,
    owner: &Owner,
    source_batch_id: SourceBatchId,
    payload: &CodeChunkV1,
    source_file_revision: MemoryId,
    source_commit: Option<MemoryId>,
) -> Result<DerivedOutcome, IngestError> {
    let memory_id = code_slice_memory_id(payload, source_file_revision);
    let output_memory_id = MemoryId::new(memory_id);
    let mut tx = pool.begin().await?;
    let text = render_code_slice(payload);
    let draft = DerivedDraft {
        memory_id,
        owner: *owner,
        kind: EntityKind::Abstraction,
        schema_id: <CodeChunkV1 as AbstractionPayload>::schema_id(),
        schema_version: SchemaVersion::new(CodeChunkV1::SCHEMA_VERSION),
        text,
        operator_kind: MemoryOperatorKind::FtoA,
        operator_id: code_slice_operator_id(),
        input_contract_id: code_slice_input_contract_id(payload, source_file_revision),
        source_batch_id: Some(source_batch_id),
        model_id: CODE_SLICE_OPERATOR_MODEL,
        prompt_version: CODE_SLICE_PROMPT_VERSION,
        supersedes: None,
        embedding: None,
        embedding_model_id: None,
    };
    let registry = schema_registry();
    let relation = registry
        .resolve_relation(CORE_DERIVED_FROM_RELATION)
        .ok_or_else(|| {
            IngestError::Storage(format!(
                "missing registered relation {CORE_DERIVED_FROM_RELATION}"
            ))
        })?;
    let mut edges = vec![code_slice_provenance_edge(
        owner,
        output_memory_id,
        source_file_revision,
        relation,
    )];
    if let Some(commit) = source_commit {
        edges.push(code_slice_provenance_edge(
            owner,
            output_memory_id,
            commit,
            relation,
        ));
    }
    let sidecar_payload = payload.clone();
    let outcome = append_derived_with_edges_in_tx(&mut tx, &draft, &edges, move |tx, outcome| {
        Box::pin(async move {
            sidecar_payload
                .insert_memory_sidecar(tx, outcome.memory_id)
                .await
        })
    })
    .await?;
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

fn code_slice_provenance_edge<'a>(
    owner: &'a Owner,
    code_slice: MemoryId,
    source_fact: MemoryId,
    relation: RegisteredRelation<'a>,
) -> DerivedEdgeSpec<'a> {
    DerivedEdgeSpec {
        owner,
        relation,
        source_kind: EntityKind::Abstraction,
        source_memory_id: code_slice,
        target_kind: EntityKind::Fact,
        target_memory_id: source_fact,
        authorship_kind: EdgeAuthorshipKind::OperatorFtoA,
        authorship_owner_memory_id: Some(code_slice),
        sidecar_payload: None,
    }
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
