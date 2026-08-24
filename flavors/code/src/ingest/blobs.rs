use std::collections::HashMap;

use proxima_core::storage_ports::OwnerWritePermit;
use proxima_core::verbs::fact_ingest::{CitationSpec, FactIngestOutcome};
use proxima_core::verbs::query::SidecarAtom;
use proxima_core::{
    AbstractionPayload, AuthorDerivedAuthorizedOutcome, AuthorDerivedRequestInput, AuthzContext,
    EdgeEndpoint, Engine, EntityKind, InputContractId, MemoryId, MemoryOperatorKind, OperatorId,
    Owner, SchemaVersion, SidecarPayload, SourceBatchId, TypedFactIngest,
};
use proxima_storage_pg::query::ChunkSeriesHead;
use sqlx::PgPool;
use uuid::Uuid;

use crate::payloads::{CodeChunkV1, CommitV1, FileRevisionV1};
use crate::store::CodeFlavorStore;

use super::IngestError;
use super::schemas::{
    CODE_BLOB_SCHEMA, CODE_BLOB_WHOLE_SCHEMA, CODE_COMMIT_OBJECT_SCHEMA, CODE_COMMIT_WHOLE_SCHEMA,
    LOCAL_GIT_SOURCE_ID,
};

const CODE_SLICE_OPERATOR_MODEL: &str = "proxima-code/local-git-source";

/// Version of the code-slice derivation, carried in the input-contract id.
///
/// The series handle is looked up per `(owner, repo, path, index)` and
/// reused; each revision is a new `t`. Bump this prefix when chunker
/// boundaries, `render_code_slice`, or stored fields change — the
/// contract id must not collide with a previous derivation of the same
/// position. A HEAD snapshot still skips unchanged blobs; re-derive
/// after a chunker change is `proxima-code_erase_repo` plus a fresh
/// register.
const CODE_SLICE_IDENTITY: &[u8] = b"proxima-code/code-slice:local-git-file-facts-v3";
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
        b"proxima-code/local-git-source:code-slice-v3",
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

/// Positional key for the input-contract id. The series handle is not
/// derived from this — it is looked up per owner + natural key.
fn code_slice_identity_key(payload: &CodeChunkV1, source_file_revision: MemoryId) -> Vec<u8> {
    let mut key = Vec::with_capacity(128 + payload.file_path.len());
    key.extend_from_slice(CODE_SLICE_IDENTITY);
    key.push(0);
    key.extend_from_slice(payload.repo_id.as_bytes());
    key.push(0);
    key.extend_from_slice(payload.file_path.as_bytes());
    key.push(0);
    key.extend_from_slice(&payload.chunk_index.to_be_bytes());
    let _ = source_file_revision;
    key
}

/// Close a `source_batch` opened by the typed-ingest helpers under a
/// LocalGitSource poll. Idempotent. Maps `NotFound` to `Ok(())` so
/// callers can safely call this after no-op polls (no events → no
/// batch row was ever inserted).
pub async fn close_local_git_batch(
    _pool: &PgPool,
    _permit: &OwnerWritePermit,
    _source_batch_id: SourceBatchId,
) -> Result<(), IngestError> {
    Ok(())
}

async fn ingest_local_git_fact<P>(
    engine: &Engine,
    authz: &AuthzContext,
    source_batch_id: SourceBatchId,
    payload: &P,
    citation: CitationSpec,
    observed_at: time::OffsetDateTime,
) -> Result<FactIngestOutcome, IngestError>
where
    P: proxima_core::FactPayload + Clone,
{
    engine
        .ingest_typed_fact_with(
            authz,
            TypedFactIngest::new(LOCAL_GIT_SOURCE_ID, payload)
                .source_batch_id(source_batch_id)
                .observed_at(observed_at)
                .citation(citation),
        )
        .await
        .map_err(IngestError::from)
}

/// Current series handle for this owner's chunk at `(repo, path, index)`.
///
/// Miss after an owner transfer is expected: the series belongs to the
/// destination owner. The caller mints a new handle.
pub async fn existing_code_chunk_handle(
    engine: &Engine,
    authz: &AuthzContext,
    owner: proxima_core::Owner,
    repo_id: uuid::Uuid,
    file_path: &str,
    chunk_index: u32,
) -> Result<Option<uuid::Uuid>, IngestError> {
    let chunk_index = i32::try_from(chunk_index).unwrap_or(i32::MAX);
    Ok(engine
        .owned_series_handle(
            authz,
            owner,
            &<CodeChunkV1 as AbstractionPayload>::schema_id(),
            <CodeChunkV1 as AbstractionPayload>::sidecar_table(),
            &[
                ("repo_id", SidecarAtom::Uuid(repo_id)),
                ("file_path", SidecarAtom::Text(file_path.to_string())),
                ("chunk_index", SidecarAtom::I32(chunk_index)),
            ],
        )
        .await?)
}

/// Reuse listed series handles, or mint. One file per call.
///
/// # Errors
///
/// `Storage` when payloads mix `(repo_id, file_path)` or a listed
/// `chunk_index` is not a `u32`.
pub fn assign_code_chunk_handles(
    heads: &[ChunkSeriesHead],
    payloads: &[CodeChunkV1],
) -> Result<Vec<Uuid>, IngestError> {
    if let Some(first) = payloads.first() {
        for payload in payloads {
            if payload.repo_id != first.repo_id || payload.file_path != first.file_path {
                return Err(IngestError::Storage(
                    "code slice resolve requires one (repo_id, file_path)".into(),
                ));
            }
        }
    }
    let mut by_index = HashMap::with_capacity(heads.len());
    for head in heads {
        let index = u32::try_from(head.chunk_index).map_err(|err| {
            IngestError::Storage(format!(
                "invalid code chunk index {}: {err}",
                head.chunk_index
            ))
        })?;
        if by_index.insert(index, head.handle).is_some() {
            return Err(IngestError::Storage(format!(
                "duplicate chunk_index {index} at current head"
            )));
        }
    }
    Ok(payloads
        .iter()
        .map(|payload| {
            by_index
                .get(&payload.chunk_index)
                .copied()
                .unwrap_or_else(Uuid::now_v7)
        })
        .collect())
}

/// One handle per payload: one file listing, then [`assign_code_chunk_handles`].
///
/// Call once per file before filling `calls` and writing, so intra-file
/// callees share the same series ids the drafts will use.
pub async fn resolve_code_chunk_handles(
    store: &CodeFlavorStore,
    owner: Owner,
    payloads: &[CodeChunkV1],
) -> Result<Vec<Uuid>, IngestError> {
    if payloads.is_empty() {
        return Ok(Vec::new());
    }
    let first = &payloads[0];
    for payload in payloads {
        if payload.repo_id != first.repo_id || payload.file_path != first.file_path {
            return Err(IngestError::Storage(
                "code slice resolve requires one (repo_id, file_path)".into(),
            ));
        }
    }
    let heads = store
        .owned_chunk_series_heads(owner, first.repo_id, &first.file_path)
        .await
        .map_err(|err| IngestError::Storage(err.to_string()))?;
    assign_code_chunk_handles(&heads, payloads)
}

/// Atomic Fact + sidecar write for `commit-v1`. Cites the commit
/// object (keyed by blake3 of the commit sha) with a "whole-commit"
/// CitationMapping.
pub async fn ingest_commit(
    engine: &Engine,
    authz: &AuthzContext,
    source_batch_id: SourceBatchId,
    payload: &CommitV1,
    observed_at: time::OffsetDateTime,
) -> Result<FactIngestOutcome, IngestError> {
    ingest_local_git_fact(
        engine,
        authz,
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
    engine: &Engine,
    authz: &AuthzContext,
    source_batch_id: SourceBatchId,
    payload: &FileRevisionV1,
    observed_at: time::OffsetDateTime,
) -> Result<FactIngestOutcome, IngestError> {
    ingest_local_git_fact(
        engine,
        authz,
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

/// Atomic derived code-slice Abstractions for one file revision, plus the
/// index rows their own declarations imply: `origin` entries to the source
/// file-revision Fact and, when available, the source commit Fact, and one
/// `reference` entry per callee each chunk's payload names.
///
/// The whole file lands as one group in one transaction, because the group
/// refers to itself. Series handles are resolved first
/// ([`resolve_code_chunk_handles`]) so callees can be named before insert.
///
/// The flavor names endpoints and content; it never names a kind. Origins are
/// what these writes say they were made from, references are read back off
/// the payloads by [`CodeChunkV1::references`], and both land inside this
/// transaction — which is what makes a re-ingest re-assert the same rows
/// instead of minting new ones.
///
/// Chunking is deterministic F→A operator work over file/blob Facts;
/// callers must materialize all batch Facts before invoking this helper.
/// This helper deliberately does not write an event, source batch, or
/// Fact citation.
pub async fn append_code_slices(
    engine: &Engine,
    authz: &AuthzContext,
    owner: Owner,
    store: &CodeFlavorStore,
    payloads: &[CodeChunkV1],
    source_file_revision: MemoryId,
    source_commit: Option<MemoryId>,
) -> Result<Vec<AuthorDerivedAuthorizedOutcome>, IngestError> {
    let handles = resolve_code_chunk_handles(store, owner, payloads).await?;
    append_code_slices_with_handles(
        engine,
        authz,
        owner,
        payloads,
        source_file_revision,
        source_commit,
        &handles,
    )
    .await
}

/// [`append_code_slices`] when the caller already resolved series handles
/// (intra-file call naming).
pub async fn append_code_slices_with_handles(
    engine: &Engine,
    authz: &AuthzContext,
    owner: Owner,
    payloads: &[CodeChunkV1],
    source_file_revision: MemoryId,
    source_commit: Option<MemoryId>,
    handles: &[uuid::Uuid],
) -> Result<Vec<AuthorDerivedAuthorizedOutcome>, IngestError> {
    if payloads.len() != handles.len() {
        return Err(IngestError::Storage(
            "code slice handle count must match payload count".into(),
        ));
    }
    if payloads.is_empty() {
        return Ok(Vec::new());
    }
    let mut origins = vec![EdgeEndpoint::memory(EntityKind::Fact, source_file_revision)];
    if let Some(commit) = source_commit {
        origins.push(EdgeEndpoint::memory(EntityKind::Fact, commit));
    }
    // Embed every slice before BEGIN. Intra-file calls are sidecar data,
    // not kernel pins (`CodeChunkV1::references` is empty); the one
    // transaction is atomicity of the file group, not sibling visibility.
    let reqs = payloads
        .iter()
        .zip(handles)
        .map(|(payload, handle)| AuthorDerivedRequestInput {
            memory_id: MemoryId::new(*handle),
            owner,
            kind: EntityKind::Abstraction,
            text: render_code_slice(payload),
            schema_id: <CodeChunkV1 as AbstractionPayload>::schema_id(),
            schema_version: SchemaVersion::new(CodeChunkV1::SCHEMA_VERSION),
            operator_kind: MemoryOperatorKind::FtoA,
            operator_id: code_slice_operator_id(),
            input_contract_id: code_slice_input_contract_id(payload, source_file_revision),
            model_id: CODE_SLICE_OPERATOR_MODEL,
            sidecar_payload: SidecarPayload::abstraction(payload.clone()),
            derived_from: &origins,
            extra_refs: &[],
            supersedes: None,
            // The chunk schema's contract PINS its configuration, so the
            // projection statement carries it as a literal and reads no
            // language from the write. A value here would be discarded,
            // and a discarded value is a second place to keep the pin in
            // sync with.
            lexical_language: None,
        });
    let mut uow = engine.unit_of_work(authz).await?;
    let outcomes = uow.author_derived_all(reqs).await?;
    uow.commit().await?;
    Ok(outcomes)
}

/// One code slice, on its own. The tombstone path writes a single chunk that
/// declares no calls, so it needs no group.
pub async fn append_code_slice(
    engine: &Engine,
    authz: &AuthzContext,
    owner: Owner,
    payload: &CodeChunkV1,
    source_file_revision: MemoryId,
    source_commit: Option<MemoryId>,
) -> Result<AuthorDerivedAuthorizedOutcome, IngestError> {
    let handle = existing_code_chunk_handle(
        engine,
        authz,
        owner,
        payload.repo_id,
        &payload.file_path,
        payload.chunk_index,
    )
    .await?
    .unwrap_or_else(Uuid::now_v7);
    let outcomes = append_code_slices_with_handles(
        engine,
        authz,
        owner,
        std::slice::from_ref(payload),
        source_file_revision,
        source_commit,
        &[handle],
    )
    .await?;
    outcomes
        .into_iter()
        .next()
        .ok_or_else(|| IngestError::Storage("code slice batch returned no outcome".into()))
}

/// Deterministic input-contract material for this position. Not the
/// series handle — resolve that with [`resolve_code_chunk_handles`].
#[must_use]
pub fn code_slice_memory_id_for(payload: &CodeChunkV1, source_file_revision: MemoryId) -> MemoryId {
    MemoryId::new(uuid::Uuid::new_v5(
        &CODE_SLICE_NAMESPACE,
        &code_slice_identity_key(payload, source_file_revision),
    ))
}

/// The chunk's rendered form: a `path:start-end` header line followed by
/// the chunk body.
///
/// The render is what the embedding pipeline embeds
/// (`fact_embeddings::text::load_embedding_text`). Header plus body: the header
/// makes a retrieved chunk actionable (file and lines) and carries lexical
/// signal from the path; the body is what a question about the code matches.
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

#[cfg(test)]
mod assign_tests {
    use super::{CodeChunkV1, assign_code_chunk_handles};
    use crate::payloads::FileState;
    use proxima_storage_pg::query::ChunkSeriesHead;
    use uuid::Uuid;

    fn chunk(repo: Uuid, path: &str, index: u32) -> CodeChunkV1 {
        CodeChunkV1 {
            repo_id: repo,
            file_path: path.to_string(),
            chunk_index: index,
            text: String::new(),
            language: None,
            chunk_type: "block".into(),
            byte_range_start: 0,
            byte_range_end: 0,
            line_range_start: 0,
            line_range_end: 0,
            state: FileState::Present,
            calls: Vec::new(),
        }
    }

    #[test]
    fn assign_reuses_listed_handle_and_mints_unknown_index() {
        let repo = Uuid::now_v7();
        let listed = Uuid::now_v7();
        let heads = [ChunkSeriesHead {
            chunk_index: 0,
            handle: listed,
            state: "Present".into(),
        }];
        let payloads = [chunk(repo, "a.rs", 0), chunk(repo, "a.rs", 1)];
        let handles = assign_code_chunk_handles(&heads, &payloads).expect("assign");
        assert_eq!(handles[0], listed);
        assert_ne!(handles[1], listed);
    }

    #[test]
    fn assign_rejects_mixed_file() {
        let repo = Uuid::now_v7();
        let payloads = [chunk(repo, "a.rs", 0), chunk(repo, "b.rs", 0)];
        let err = assign_code_chunk_handles(&[], &payloads).expect_err("mixed");
        assert!(err.to_string().contains("(repo_id, file_path)"), "{err}");
    }
}
