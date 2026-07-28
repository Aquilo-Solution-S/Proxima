use proxima_core::storage_ports::OwnerWritePermit;
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

/// Version of the code-slice derivation, carried in chunk identity.
///
/// A code chunk's `memory_id` is a v5 UUID over its position — source file
/// revision, repo, path, chunk index, state — deliberately stable across
/// re-ingests, because chunking the same blob twice must not mint a second
/// memory. `append_derived_with_edges_in_tx` inserts `ON CONFLICT
/// (memory_id) DO NOTHING`, so without this prefix a chunker or render change
/// would derive every file to exactly the same ids and silently discard the
/// new text.
///
/// Bump it whenever the bytes a chunk carries stop being a pure function of
/// its position: chunker boundaries, `render_code_slice`, the payload's
/// stored fields. v2 is v0.0.7 — comments joined the chunk text, and the
/// render gained the chunk body.
///
/// Bumping it does **not** make an existing index re-derive itself. A HEAD
/// snapshot skips files whose blob hash has not moved, and that skip cannot
/// simply be lifted: `validate_ftoa_input_batch` requires a derived
/// Abstraction to carry the same `source_batch_id` as the Facts it came from,
/// so re-deriving an unchanged file would stamp new chunks with a batch its
/// already-receipted Fact does not belong to. The supported path is
/// `proxima-code_erase_repo` followed by a fresh register and ingest, which
/// produces new Facts in new batches. What this constant guarantees is
/// narrower and still worth having: the two derivations can never collide on
/// an id, so a stale chunk cannot masquerade as a current one.
const CODE_SLICE_IDENTITY: &[u8] = b"proxima-code/code-slice:local-git-file-facts-v2";
const CODE_SLICE_PROMPT_VERSION: &str = "code-slice-v2";

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
        b"proxima-code/local-git-source:code-slice-v2",
    ))
}

fn code_slice_input_contract_id(
    payload: &CodeChunkV1,
    source_file_revision: MemoryId,
) -> InputContractId {
    InputContractId::new(uuid::Uuid::new_v5(
        &CODE_SLICE_INPUT_CONTRACT_NAMESPACE,
        &code_slice_identity_key(payload, source_file_revision),
    ))
}

/// The positional key both code-slice ids are derived from, prefixed with
/// [`CODE_SLICE_IDENTITY`] so a derivation change reaches both of them.
fn code_slice_identity_key(payload: &CodeChunkV1, source_file_revision: MemoryId) -> Vec<u8> {
    let mut key = Vec::with_capacity(128 + payload.file_path.len());
    key.extend_from_slice(CODE_SLICE_IDENTITY);
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
    key
}

/// Close a `source_batch` opened by the typed-ingest helpers under a
/// LocalGitSource poll. Idempotent. Maps `NotFound` to `Ok(())` so
/// callers can safely call this after no-op polls (no events → no
/// batch row was ever inserted).
pub async fn close_local_git_batch(
    pool: &PgPool,
    permit: &OwnerWritePermit,
    source_batch_id: SourceBatchId,
) -> Result<(), IngestError> {
    match proxima_storage_pg::verbs::close_batch::close_batch(pool, permit, source_batch_id).await {
        Ok(_) | Err(proxima_core::StorageError::NotFound) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

fn local_git_context(
    permit: &OwnerWritePermit,
    source_batch_id: SourceBatchId,
    observed_at: time::OffsetDateTime,
) -> FactIngestContext<'_> {
    FactIngestContext::new(permit, LOCAL_GIT_SOURCE_ID, source_batch_id).observed_at(observed_at)
}

async fn ingest_local_git_fact<P>(
    pool: &PgPool,
    permit: &OwnerWritePermit,
    source_batch_id: SourceBatchId,
    payload: &P,
    citation: CitationSpec,
    observed_at: time::OffsetDateTime,
) -> Result<FactIngestOutcome, IngestError>
where
    P: FactPayload + PgMemorySidecar + Clone,
{
    let ctx = local_git_context(permit, source_batch_id, observed_at);
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
    permit: &OwnerWritePermit,
    source_batch_id: SourceBatchId,
    payload: &CommitV1,
    observed_at: time::OffsetDateTime,
) -> Result<FactIngestOutcome, IngestError> {
    ingest_local_git_fact(
        pool,
        permit,
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
    permit: &OwnerWritePermit,
    source_batch_id: SourceBatchId,
    payload: &FileRevisionV1,
    observed_at: time::OffsetDateTime,
) -> Result<FactIngestOutcome, IngestError> {
    ingest_local_git_fact(
        pool,
        permit,
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
    permit: &OwnerWritePermit,
    source_batch_id: SourceBatchId,
    payload: &CodeChunkV1,
    source_file_revision: MemoryId,
    source_commit: Option<MemoryId>,
) -> Result<DerivedOutcome, IngestError> {
    let owner = permit.owner();
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
        // Chunks pin english on every surface (see CODE_LEXICAL_LANGUAGE):
        // this stamps the owning memories row, the sidecar mirrors the pin
        // via its column default, and passing it here (not None) keeps
        // 'english' registered as an active language on every ingest.
        lexical_language: Some(crate::payloads::CODE_LEXICAL_LANGUAGE),
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
    let outcome =
        append_derived_with_edges_in_tx(&mut tx, permit, &draft, &edges, move |tx, outcome| {
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
    uuid::Uuid::new_v5(
        &CODE_SLICE_NAMESPACE,
        &code_slice_identity_key(payload, source_file_revision),
    )
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

/// The chunk's rendered form: a `path:start-end` header line followed by
/// the chunk body.
///
/// The render is `memories.text`, and `memories.text` is what the embedding
/// pipeline embeds (`fact_embeddings::text::load_embedding_text`) and what
/// `memories.search_tsv` is generated from. While this returned the header
/// alone, every code-chunk embedding in a corpus was a 1024-d encoding of a
/// file path and two line numbers, and `core_search_memories` could only
/// ever retrieve code whose *filename* resembled the question. Measured on
/// this repository's own index, asking where an over-limit embedding input
/// gets split returned five chunks of `flavors/code/src/chunker.rs` — the
/// path contains "chunker" — with a lexical score of exactly 0.0 on every
/// row, and never returned `crates/core/src/llm.rs`, which is the answer.
///
/// The header stays because it is what makes a retrieved chunk actionable:
/// the agent needs to know which file and lines it is looking at, and the
/// path also carries real lexical signal.
fn render_code_slice(payload: &CodeChunkV1) -> String {
    match payload.state {
        crate::payloads::FileState::Present => format!(
            "{}:{}-{}\n{}",
            payload.file_path, payload.line_range_start, payload.line_range_end, payload.text
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
