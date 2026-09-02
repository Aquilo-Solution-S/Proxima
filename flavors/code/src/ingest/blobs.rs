use std::collections::HashMap;

use proxima_core::verbs::fact_ingest::{CitationSpec, FactIngestOutcome};
use proxima_core::verbs::query::SidecarAtom;
use proxima_core::{
    AbstractionPayload, AuthorDerivedAuthorizedOutcome, AuthorDerivedRequestInput, AuthzContext,
    EdgeEndpoint, Engine, EntityKind, InputContractId, MemoryId, MemoryOperatorKind, OperatorId,
    Owner, SchemaVersion, SidecarPayload, TypedFactIngest,
};
use proxima_storage_pg::query::ChunkSeriesHead;
use uuid::Uuid;

use crate::calls::{ExtractedCall, ExtractedDefinition};
use crate::chunker::Chunk;
use crate::payloads::{
    CodeCallSiteV1, CodeCallV1, CodeChunkV1, CommitV1, FileRevisionV1, FileState,
};
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

/// One repo-scoped Fact.
///
/// No fence is taken here and none may be: `CommitV1` and `FileRevisionV1`
/// declare `CODE_REPO_SCOPE`, so the Engine takes the `code-repo` fence
/// shared and re-asks whether the repository is registered inside this
/// write transaction, before the admission's handle and `t` locks. Taking
/// it a second time from the flavor would add nothing except a second place
/// to forget it — and forgetting it was the whole defect, since a host
/// writing the same payload through `Engine` never reached this function.
async fn ingest_local_git_fact<P>(
    engine: &Engine,
    authz: &AuthzContext,
    payload: &P,
    citation: CitationSpec,
    observed_at: time::OffsetDateTime,
) -> Result<FactIngestOutcome, IngestError>
where
    P: proxima_core::FactPayload + Clone,
{
    Ok(engine
        .ingest_typed_fact_with(
            authz,
            TypedFactIngest::new(LOCAL_GIT_SOURCE_ID, payload)
                .observed_at(observed_at)
                .citation(citation),
        )
        .await?)
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
    payload: &CommitV1,
    observed_at: time::OffsetDateTime,
) -> Result<FactIngestOutcome, IngestError> {
    ingest_local_git_fact(
        engine,
        authz,
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
    payload: &FileRevisionV1,
    observed_at: time::OffsetDateTime,
) -> Result<FactIngestOutcome, IngestError> {
    ingest_local_git_fact(
        engine,
        authz,
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
/// callers must materialize every input Fact before invoking this
/// helper. This helper deliberately does not write an event or a Fact
/// citation.
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
    let Some(first) = payloads.first() else {
        return Ok(Vec::new());
    };
    let repo_id = first.repo_id;
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
    // One repository per group. The scope fence is a per-repository lane
    // and the group's handles are resolved per repository, so a batch that
    // spanned two of them would be reasoning about neither.
    if payloads.iter().any(|payload| payload.repo_id != repo_id) {
        return Err(IngestError::Storage(
            "code slice batch requires one repo_id".into(),
        ));
    }
    let mut uow = engine.unit_of_work(authz).await?;
    // No fence here: `CodeChunkV1` declares `CODE_REPO_SCOPE`, so the first
    // derived write takes it in this transaction before the group's handles
    // and `t`s, and every later member of the group finds it already held.
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

/// Chunk info for call resolution.
#[derive(Debug, Clone)]
pub(crate) struct ChunkInfo {
    pub(crate) memory_id: MemoryId,
    pub(crate) payload: CodeChunkV1,
    pub(crate) item_names: Vec<String>,
}

/// Build this file's slice payloads and pair each with the series handle
/// it will be written under.
///
/// Reuse listed series handles so intra-file calls can name callees
/// before insert. Mint only on miss.
///
/// # Errors
///
/// `Storage` when [`assign_code_chunk_handles`] rejects the batch.
pub(crate) fn plan_file_chunks(
    repo_id: Uuid,
    file_path: &str,
    chunks: &[Chunk],
    definitions: &[ExtractedDefinition],
    heads: &[ChunkSeriesHead],
) -> Result<Vec<ChunkInfo>, IngestError> {
    let mut bare_payloads: Vec<CodeChunkV1> = Vec::new();
    for (idx, chunk) in chunks.iter().enumerate() {
        let chunk_index = u32::try_from(idx).unwrap_or(u32::MAX);
        bare_payloads.push(CodeChunkV1 {
            repo_id,
            file_path: file_path.to_string(),
            chunk_index,
            text: chunk.text.clone(),
            language: chunk.language.map(str::to_string),
            chunk_type: chunk.chunk_type.to_string(),
            byte_range_start: chunk.byte_range_start,
            byte_range_end: chunk.byte_range_end,
            line_range_start: chunk.line_range_start,
            line_range_end: chunk.line_range_end,
            state: FileState::Present,
            calls: Vec::new(),
        });
    }
    let handles = assign_code_chunk_handles(heads, &bare_payloads)?;
    let mut file_chunks: Vec<ChunkInfo> = Vec::new();
    for (payload, handle) in bare_payloads.into_iter().zip(handles) {
        let memory_id = MemoryId::new(handle);
        let item_names: Vec<String> = definitions
            .iter()
            .filter(|d| {
                d.byte_start >= payload.byte_range_start && d.byte_end <= payload.byte_range_end
            })
            .map(|d| d.name.clone())
            .collect();
        file_chunks.push(ChunkInfo {
            memory_id,
            payload,
            item_names,
        });
    }
    Ok(file_chunks)
}

/// Resolve each call into the caller/callee chunk pair and record it in the
/// *caller's payload*. Resolution is intra-file v1; cross-file calls wait for
/// an indexed name table. Ten sites into the same callee are ten entries here
/// and one index row — the multiplicity belongs to the node
/// (docs/16 §The Model).
pub(crate) fn resolve_intra_file_calls(calls: &[ExtractedCall], file_chunks: &mut [ChunkInfo]) {
    for call in calls {
        let Some(caller_index) = file_chunks
            .iter()
            .enumerate()
            .filter(|(_, c)| {
                c.payload.byte_range_start <= call.byte_start
                    && c.payload.byte_range_end >= call.byte_end
            })
            .max_by_key(|(_, c)| c.payload.byte_range_start)
            .map(|(index, _)| index)
        else {
            continue;
        };
        let Some(callee_memory_id) = file_chunks
            .iter()
            .find(|c| c.item_names.iter().any(|n| n == &call.callee_name))
            .map(|c| c.memory_id)
        else {
            continue;
        };
        // A chunk that calls itself is not a connection between two
        // things, and the index refuses the row outright.
        if file_chunks[caller_index].memory_id == callee_memory_id {
            continue;
        }
        let site = CodeCallSiteV1 {
            byte_start: call.byte_start,
            byte_end: call.byte_end,
            callee_name: call.callee_name.clone(),
            is_dynamic: call.is_dynamic,
        };
        let calls = &mut file_chunks[caller_index].payload.calls;
        match calls
            .iter_mut()
            .find(|existing| existing.callee_memory_id == callee_memory_id.into_inner())
        {
            Some(existing) => existing.sites.push(site),
            None => calls.push(CodeCallV1 {
                callee_memory_id: callee_memory_id.into_inner(),
                sites: vec![site],
            }),
        }
    }
}

/// Build a tombstone `CodeChunkV1` payload for a `(repo, path, idx)`.
/// `language` is `None` when the file itself was deleted; for shrink
/// tombstones the file's current language is preserved so the head
/// view stays self-consistent.
pub(crate) fn tombstone_chunk(
    repo_id: Uuid,
    path: &str,
    chunk_index: u32,
    language: Option<String>,
) -> CodeChunkV1 {
    CodeChunkV1 {
        repo_id,
        file_path: path.to_string(),
        chunk_index,
        text: String::new(),
        language,
        chunk_type: "block".into(),
        byte_range_start: 0,
        byte_range_end: 0,
        line_range_start: 0,
        line_range_end: 0,
        state: FileState::Tombstone,
        // A tombstone slice asserts that the position is gone. It calls
        // nothing, so it declares nothing and its index rows disappear
        // with it.
        calls: Vec::new(),
    }
}

#[cfg(test)]
mod resolve_tests {
    use super::{ChunkInfo, resolve_intra_file_calls};
    use crate::calls::ExtractedCall;
    use crate::payloads::{CodeChunkV1, FileState};
    use proxima_core::MemoryId;
    use uuid::Uuid;

    fn id(byte: u8) -> Uuid {
        Uuid::from_bytes([byte; 16])
    }

    /// A planned chunk at `[start, end]` — the containment test is
    /// inclusive on both ends, matching the payload's stored byte range.
    fn chunk(marker: u8, start: u32, end: u32, item_names: &[&str]) -> ChunkInfo {
        ChunkInfo {
            memory_id: MemoryId::new(id(marker)),
            payload: CodeChunkV1 {
                repo_id: Uuid::nil(),
                file_path: "a.rs".into(),
                chunk_index: u32::from(marker),
                text: String::new(),
                language: None,
                chunk_type: "block".into(),
                byte_range_start: start,
                byte_range_end: end,
                line_range_start: 0,
                line_range_end: 0,
                state: FileState::Present,
                calls: Vec::new(),
            },
            item_names: item_names.iter().map(|n| (*n).to_string()).collect(),
        }
    }

    fn call(start: u32, end: u32, callee_name: &str) -> ExtractedCall {
        ExtractedCall {
            byte_start: start,
            byte_end: end,
            callee_name: callee_name.to_string(),
            is_dynamic: false,
        }
    }

    /// Every recorded connection as `(caller slice position, callee marker,
    /// call-site byte starts in payload order)`.
    fn recorded(file_chunks: &[ChunkInfo]) -> Vec<(usize, u8, Vec<u32>)> {
        let mut out = Vec::new();
        for (position, info) in file_chunks.iter().enumerate() {
            for entry in &info.payload.calls {
                out.push((
                    position,
                    entry.callee_memory_id.as_bytes()[0],
                    entry.sites.iter().map(|site| site.byte_start).collect(),
                ));
            }
        }
        out
    }

    /// Two chunks that share no bytes; the call sits wholly inside the
    /// second one.
    #[test]
    fn disjoint_chunks_resolve_the_containing_caller() {
        let mut chunks = vec![chunk(1, 0, 10, &["callee"]), chunk(2, 10, 20, &[])];
        resolve_intra_file_calls(&[call(12, 15, "callee")], &mut chunks);
        assert_eq!(recorded(&chunks), vec![(1, 1, vec![12])]);
    }

    /// Containment is all-or-nothing: a call whose bytes straddle the
    /// boundary belongs to neither chunk and is dropped rather than
    /// attributed to a chunk that holds only part of it.
    #[test]
    fn call_crossing_a_chunk_boundary_is_dropped() {
        let mut chunks = vec![chunk(1, 0, 10, &["callee"]), chunk(2, 10, 20, &[])];
        resolve_intra_file_calls(&[call(8, 12, "callee")], &mut chunks);
        assert_eq!(recorded(&chunks), Vec::new());
    }

    /// Chunk coverage of a file can have holes; a call inside one has no
    /// caller.
    #[test]
    fn call_in_a_gap_between_chunks_is_dropped() {
        let mut chunks = vec![chunk(1, 0, 10, &["callee"]), chunk(2, 20, 30, &[])];
        resolve_intra_file_calls(&[call(12, 15, "callee")], &mut chunks);
        assert_eq!(recorded(&chunks), Vec::new());
    }

    /// The chunker's fallback path gives every window the whole blob as its
    /// range, so every chunk contains every call. The tie-break is the
    /// *last* of the equal-start run, not the first.
    #[test]
    fn identical_ranges_resolve_to_the_last_chunk() {
        let mut chunks = vec![
            chunk(1, 0, 100, &["callee"]),
            chunk(2, 0, 100, &[]),
            chunk(3, 0, 100, &[]),
        ];
        resolve_intra_file_calls(&[call(5, 9, "callee")], &mut chunks);
        assert_eq!(recorded(&chunks), vec![(2, 1, vec![5])]);
    }

    /// Defensive: the AST path emits disjoint spans, but if two chunks ever
    /// nest, the innermost — the one with the largest start — is the caller.
    #[test]
    fn nested_chunks_resolve_to_the_innermost_start() {
        let mut chunks = vec![chunk(1, 0, 100, &["callee"]), chunk(2, 10, 20, &[])];
        resolve_intra_file_calls(&[call(12, 15, "callee")], &mut chunks);
        assert_eq!(recorded(&chunks), vec![(1, 1, vec![12])]);
    }

    /// Adjacent chunks share the boundary byte; a call starting exactly on
    /// it belongs to the later chunk, because the containment test is
    /// inclusive and the largest start wins.
    #[test]
    fn call_starting_on_a_shared_boundary_takes_the_later_chunk() {
        let mut chunks = vec![chunk(1, 0, 10, &["callee"]), chunk(2, 10, 20, &[])];
        resolve_intra_file_calls(&[call(10, 12, "callee")], &mut chunks);
        assert_eq!(recorded(&chunks), vec![(1, 1, vec![10])]);
    }

    /// One name can be defined in more than one chunk (a re-export, a
    /// duplicated helper, a merged span). The first chunk in slice order
    /// that declares it wins.
    #[test]
    fn duplicate_callee_name_resolves_to_the_first_chunk() {
        let mut chunks = vec![
            chunk(1, 0, 10, &["callee"]),
            chunk(2, 10, 20, &[]),
            chunk(3, 20, 30, &["callee"]),
        ];
        resolve_intra_file_calls(&[call(12, 15, "callee")], &mut chunks);
        assert_eq!(recorded(&chunks), vec![(1, 1, vec![12])]);
    }

    /// A chunk calling a name it defines itself is not a connection between
    /// two things, and the index refuses the row.
    #[test]
    fn self_call_records_nothing() {
        let mut chunks = vec![chunk(1, 0, 10, &[]), chunk(2, 10, 20, &["callee"])];
        resolve_intra_file_calls(&[call(12, 15, "callee")], &mut chunks);
        assert_eq!(recorded(&chunks), Vec::new());
    }

    /// Resolution is intra-file: a name no chunk in this file defines is
    /// left unresolved rather than guessed at.
    #[test]
    fn unknown_callee_records_nothing() {
        let mut chunks = vec![chunk(1, 0, 10, &["other"]), chunk(2, 10, 20, &[])];
        resolve_intra_file_calls(&[call(12, 15, "callee")], &mut chunks);
        assert_eq!(recorded(&chunks), Vec::new());
    }

    /// Ten sites into one callee are ten entries in one connection: the
    /// multiplicity belongs to the node, and the sites keep call order.
    #[test]
    fn repeated_calls_share_one_entry_and_keep_site_order() {
        let mut chunks = vec![chunk(1, 0, 10, &["callee"]), chunk(2, 10, 20, &[])];
        resolve_intra_file_calls(
            &[call(16, 18, "callee"), call(12, 14, "callee")],
            &mut chunks,
        );
        assert_eq!(recorded(&chunks), vec![(1, 1, vec![16, 12])]);
    }

    /// Nothing depends on the chunk slice arriving sorted by start: the
    /// same call resolves to the same chunk either way.
    #[test]
    fn unsorted_chunks_resolve_the_same_caller() {
        let sorted = {
            let mut chunks = vec![
                chunk(1, 0, 10, &["callee"]),
                chunk(2, 10, 20, &[]),
                chunk(3, 20, 30, &[]),
            ];
            resolve_intra_file_calls(&[call(12, 15, "callee")], &mut chunks);
            chunks
        };
        let shuffled = {
            let mut chunks = vec![
                chunk(3, 20, 30, &[]),
                chunk(2, 10, 20, &[]),
                chunk(1, 0, 10, &["callee"]),
            ];
            resolve_intra_file_calls(&[call(12, 15, "callee")], &mut chunks);
            chunks
        };
        let by_marker = |chunks: &[ChunkInfo]| {
            let mut out: Vec<(u8, u8, Vec<u32>)> = chunks
                .iter()
                .flat_map(|info| {
                    info.payload.calls.iter().map(|entry| {
                        (
                            info.memory_id.into_inner().as_bytes()[0],
                            entry.callee_memory_id.as_bytes()[0],
                            entry.sites.iter().map(|site| site.byte_start).collect(),
                        )
                    })
                })
                .collect();
            out.sort();
            out
        };
        assert_eq!(by_marker(&sorted), vec![(2, 1, vec![12])]);
        assert_eq!(by_marker(&shuffled), by_marker(&sorted));
    }

    /// Both degenerate inputs are ordinary, not error cases: a file with no
    /// calls, and a blob that produced no chunks at all.
    #[test]
    fn empty_calls_or_chunks_are_no_ops() {
        let mut chunks = vec![chunk(1, 0, 10, &["callee"])];
        resolve_intra_file_calls(&[], &mut chunks);
        assert_eq!(recorded(&chunks), Vec::new());

        let mut empty: Vec<ChunkInfo> = Vec::new();
        resolve_intra_file_calls(&[call(12, 15, "callee")], &mut empty);
        assert!(empty.is_empty());
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
