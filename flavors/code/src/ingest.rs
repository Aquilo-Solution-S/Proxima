#![allow(clippy::missing_errors_doc, clippy::doc_markdown)]
//! Typed atomic Fact + sidecar writes for the proxima-code flavor.
//!
//! Each helper wraps `proxima_storage_pg::verbs::event_ingest::ingest_event_in_tx`
//! and the matching sidecar `INSERT` in a single Postgres transaction. On
//! idempotent replay (event_id collision) the sidecar insert is skipped —
//! the prior transaction already wrote it, and the natural-key uniqueness
//! is by construction (same payload → same event_id).
//!
//! The flavor depends on `proxima-storage-pg` for these helpers; the
//! flavor crate is no longer storage-agnostic post-M3.B.5. That coupling
//! is the v1 trade-off — keeping Fact materialization and sidecar
//! population in one tx is non-negotiable (AGENTS.md invariant 15).

use std::sync::Arc;

use proxima_core::auth::Credentials;
use proxima_core::engine::Engine;
use proxima_core::error::ProtocolError;
use proxima_core::verbs::event_ingest::{
    CitationMappingHint, CitedObjectHint, EventDraft, EventIngestOutcome,
};
use proxima_core::verbs::query::{QueryRequest, SupersessionStatus};
use proxima_core::{
    FactPayload, MemoryId, Owner, SchemaId, SchemaVersion, SourceBatchId, SourceId,
};
use proxima_storage_pg::PgStorage;
use proxima_storage_pg::verbs::event_ingest::ingest_event_in_tx;
use sqlx::PgPool;

use crate::payloads::{CodeChunkV1, CommitV1, FileRevisionV1, FileState};

/// Stable source-id namespace for `LocalGitSource` events.
pub const LOCAL_GIT_SOURCE_ID: &str = "proxima-code/local-git";

// Per-fact-type citation schema-ids (docs/11). file_revision and chunk
// Facts cite the same artefact (the file blob, keyed by content_sha256)
// via `code-blob-v1`, so the substrate's UNIQUE on
// `(owner, schema_id, content_hash)` deduplicates the CitedObject row
// and the chunks share a `cited_object_id` with their parent revision
// — no embedded MemoryId FK in the chunk payload.
//
// CitationMapping schemas differentiate the annotation:
// `whole` for facts that reference the whole artefact, `byte-range`
// for chunks. The byte/line ranges themselves stay on the chunk Fact
// payload (the substrate doesn't store typed CitationMapping bodies
// yet; the schema_id is currently a label, not a sidecar key).

/// CitedObject schema for a file blob (idempotency_key = blob's
/// `content_sha256`). Shared by `file-revision-v1` and `code-chunk-v1`.
pub const CODE_BLOB_SCHEMA: &str = "proxima-code/code-blob-v1";

/// CitedObject schema for a git commit object
/// (idempotency_key = blake3(commit sha)).
pub const CODE_COMMIT_OBJECT_SCHEMA: &str = "proxima-code/code-commit-object-v1";

/// CitationMapping for "this Fact references the whole blob"
/// (used by `file-revision-v1`).
pub const CODE_BLOB_WHOLE_SCHEMA: &str = "proxima-code/code-blob-whole-v1";

/// CitationMapping for "this Fact references a byte/line range of
/// the blob" (used by `code-chunk-v1`).
pub const CODE_BLOB_BYTE_RANGE_SCHEMA: &str = "proxima-code/code-blob-byte-range-v1";

/// CitationMapping for "this Fact references the whole commit object"
/// (used by `commit-v1`).
pub const CODE_COMMIT_WHOLE_SCHEMA: &str = "proxima-code/code-commit-whole-v1";

/// Errors raised by the typed-ingest helpers.
#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    #[error("serialization: {0}")]
    Serialize(String),
    #[error("storage: {0}")]
    Storage(String),
    #[error("protocol: {0}")]
    Protocol(#[from] ProtocolError),
}

impl From<sqlx::Error> for IngestError {
    fn from(e: sqlx::Error) -> Self {
        Self::Storage(e.to_string())
    }
}

impl From<proxima_core::StorageError> for IngestError {
    fn from(e: proxima_core::StorageError) -> Self {
        Self::Storage(e.to_string())
    }
}

/// Per-fact-type citation triple: which artefact schema, which content
/// hash deduplicates the artefact within Owner, and which annotation
/// schema labels the linkage. v1 holds schema-version at 1 across the
/// flavor.
#[derive(Clone, Copy)]
struct Citation {
    cited_object_schema: &'static str,
    content_hash: [u8; 32],
    mapping_schema: &'static str,
}

fn make_draft<P: serde::Serialize>(
    owner: &Owner,
    source_batch_id: SourceBatchId,
    payload: &P,
    schema_id: &str,
    citation: Citation,
    observed_at: time::OffsetDateTime,
) -> Result<EventDraft, IngestError> {
    let mut bytes = Vec::new();
    ciborium::ser::into_writer(payload, &mut bytes)
        .map_err(|e| IngestError::Serialize(e.to_string()))?;
    Ok(EventDraft {
        source_id: SourceId::new(LOCAL_GIT_SOURCE_ID),
        source_batch_id,
        owner: owner.clone(),
        schema_id: SchemaId::new(schema_id.into()),
        schema_version: SchemaVersion::new(1),
        payload: bytes,
        observed_at,
        occurred_at: observed_at,
        cited_object: CitedObjectHint {
            schema_id: SchemaId::new(citation.cited_object_schema.into()),
            schema_version: SchemaVersion::new(1),
            content_hash: citation.content_hash,
        },
        citation_mapping: CitationMappingHint {
            schema_id: SchemaId::new(citation.mapping_schema.into()),
            schema_version: SchemaVersion::new(1),
        },
    })
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
    let draft = make_draft(
        owner,
        source_batch_id,
        payload,
        CommitV1::SCHEMA_ID,
        Citation {
            cited_object_schema: CODE_COMMIT_OBJECT_SCHEMA,
            content_hash: blake3::hash(payload.sha.as_bytes()).into(),
            mapping_schema: CODE_COMMIT_WHOLE_SCHEMA,
        },
        observed_at,
    )?;

    let mut tx = pool.begin().await?;
    let outcome = ingest_event_in_tx(&mut tx, &draft).await?;
    if !outcome.idempotent_replay {
        sqlx::query(
            "INSERT INTO proxima_code.commit_v1 \
                (memory_id, repo_id, sha, parents, author_name, author_email, \
                 author_time, committer_name, committer_email, committer_time, \
                 message) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
        )
        .bind(outcome.memory_id.into_inner())
        .bind(payload.repo_id)
        .bind(&payload.sha)
        .bind(&payload.parents)
        .bind(&payload.author_name)
        .bind(&payload.author_email)
        .bind(payload.author_time)
        .bind(&payload.committer_name)
        .bind(&payload.committer_email)
        .bind(payload.committer_time)
        .bind(&payload.message)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(outcome)
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
    let draft = make_draft(
        owner,
        source_batch_id,
        payload,
        FileRevisionV1::SCHEMA_ID,
        Citation {
            cited_object_schema: CODE_BLOB_SCHEMA,
            content_hash: payload.content_sha256,
            mapping_schema: CODE_BLOB_WHOLE_SCHEMA,
        },
        observed_at,
    )?;

    let mut tx = pool.begin().await?;
    let outcome = ingest_event_in_tx(&mut tx, &draft).await?;
    if !outcome.idempotent_replay {
        let state_text = match payload.state {
            FileState::Present => "Present",
            FileState::Tombstone => "Tombstone",
        };
        sqlx::query(
            "INSERT INTO proxima_code.file_revision_v1 \
                (memory_id, repo_id, file_path, language, content_sha256, \
                 size_bytes, indexed_commit_sha, state) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
        )
        .bind(outcome.memory_id.into_inner())
        .bind(payload.repo_id)
        .bind(&payload.file_path)
        .bind(payload.language.as_deref())
        .bind(&payload.content_sha256[..])
        .bind(i64::try_from(payload.size_bytes).unwrap_or(i64::MAX))
        .bind(&payload.indexed_commit_sha)
        .bind(state_text)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(outcome)
}

/// Atomic Fact + sidecar write for `code-chunk-v1`. Cites the parent
/// blob (the same `cited_object_id` as the chunk's parent
/// `file-revision-v1`, by way of the substrate's UNIQUE on
/// `(owner, schema_id, content_hash)`) with a "byte-range"
/// CitationMapping. Tombstone chunks cite the null blob (`[0u8; 32]`).
///
/// `parent_blob_sha256` is the caller's responsibility — for Present
/// chunks it's the blob the chunker just operated on; for Tombstones
/// it's `[0u8; 32]`.
pub async fn ingest_code_chunk(
    pool: &PgPool,
    owner: &Owner,
    source_batch_id: SourceBatchId,
    payload: &CodeChunkV1,
    parent_blob_sha256: [u8; 32],
    observed_at: time::OffsetDateTime,
) -> Result<EventIngestOutcome, IngestError> {
    let draft = make_draft(
        owner,
        source_batch_id,
        payload,
        CodeChunkV1::SCHEMA_ID,
        Citation {
            cited_object_schema: CODE_BLOB_SCHEMA,
            content_hash: parent_blob_sha256,
            mapping_schema: CODE_BLOB_BYTE_RANGE_SCHEMA,
        },
        observed_at,
    )?;

    let mut tx = pool.begin().await?;
    let outcome = ingest_event_in_tx(&mut tx, &draft).await?;
    if !outcome.idempotent_replay {
        let state_text = match payload.state {
            FileState::Present => "Present",
            FileState::Tombstone => "Tombstone",
        };
        sqlx::query(
            "INSERT INTO proxima_code.code_chunk_v1 \
                (memory_id, repo_id, file_path, chunk_index, \
                 text, language, chunk_type, byte_range_start, byte_range_end, \
                 line_range_start, line_range_end, state) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)",
        )
        .bind(outcome.memory_id.into_inner())
        .bind(payload.repo_id)
        .bind(&payload.file_path)
        .bind(i32::try_from(payload.chunk_index).unwrap_or(i32::MAX))
        .bind(&payload.text)
        .bind(payload.language.as_deref())
        .bind(&payload.chunk_type)
        .bind(i64::from(payload.byte_range_start))
        .bind(i64::from(payload.byte_range_end))
        .bind(i64::from(payload.line_range_start))
        .bind(i64::from(payload.line_range_end))
        .bind(state_text)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(outcome)
}

/// One head row from a NK-scoped sidecar query.
#[derive(Debug, Clone)]
pub struct FileRevisionHead {
    pub memory_id: MemoryId,
    pub file_path: String,
    pub content_sha256: [u8; 32],
    pub state: FileState,
}

/// Read all current file-revision-v1 heads for a `(owner, repo_id)`
/// scope. Used by `LocalGitSource` to detect deletions and skip
/// re-ingestion of unchanged files.
pub async fn file_revision_heads(
    pool: &PgPool,
    owner: &Owner,
    repo_id: uuid::Uuid,
) -> Result<Vec<FileRevisionHead>, IngestError> {
    use proxima_core::Principal;
    let (kind, principal_id) = match &owner.principal {
        Principal::User(u) => ("User", u.into_inner()),
        Principal::Group(g) => ("Group", g.into_inner()),
    };
    let org_id = owner.org_id.into_inner();

    let rows: Vec<(uuid::Uuid, String, Vec<u8>, String)> = sqlx::query_as(
        "SELECT m.memory_id, s.file_path, s.content_sha256, s.state \
         FROM proxima_core.memories m \
         JOIN proxima_code.file_revision_v1 s USING (memory_id) \
         WHERE m.owner_principal_kind = $1 \
           AND m.owner_principal_id = $2 \
           AND m.owner_org_id = $3 \
           AND s.repo_id = $4 \
           AND NOT EXISTS ( \
                 SELECT 1 FROM proxima_core.memories m2 \
                 JOIN proxima_code.file_revision_v1 s2 USING (memory_id) \
                 WHERE m2.schema_id = m.schema_id \
                   AND m2.owner_principal_kind = m.owner_principal_kind \
                   AND m2.owner_principal_id = m.owner_principal_id \
                   AND m2.owner_org_id = m.owner_org_id \
                   AND s2.repo_id = s.repo_id \
                   AND s2.file_path = s.file_path \
                   AND m2.created_at > m.created_at \
           )",
    )
    .bind(kind)
    .bind(principal_id)
    .bind(org_id)
    .bind(repo_id)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|(mid, fp, hash, state)| {
            let mut h = [0u8; 32];
            let n = hash.len().min(32);
            h[..n].copy_from_slice(&hash[..n]);
            FileRevisionHead {
                memory_id: MemoryId::new(mid),
                file_path: fp,
                content_sha256: h,
                state: match state.as_str() {
                    "Tombstone" => FileState::Tombstone,
                    _ => FileState::Present,
                },
            }
        })
        .collect())
}

/// Read all chunk_indexes that currently have a Present head under
/// `(owner, repo_id, file_path)`. Used by `LocalGitSource` to emit
/// per-chunk Tombstones when a file is deleted.
pub async fn present_chunk_indexes(
    pool: &PgPool,
    owner: &Owner,
    repo_id: uuid::Uuid,
    file_path: &str,
) -> Result<Vec<u32>, IngestError> {
    use proxima_core::Principal;
    let (kind, principal_id) = match &owner.principal {
        Principal::User(u) => ("User", u.into_inner()),
        Principal::Group(g) => ("Group", g.into_inner()),
    };
    let org_id = owner.org_id.into_inner();

    let rows: Vec<(i32,)> = sqlx::query_as(
        "SELECT s.chunk_index \
         FROM proxima_core.memories m \
         JOIN proxima_code.code_chunk_v1 s USING (memory_id) \
         WHERE m.owner_principal_kind = $1 \
           AND m.owner_principal_id = $2 \
           AND m.owner_org_id = $3 \
           AND s.repo_id = $4 \
           AND s.file_path = $5 \
           AND s.state = 'Present' \
           AND NOT EXISTS ( \
                 SELECT 1 FROM proxima_core.memories m2 \
                 JOIN proxima_code.code_chunk_v1 s2 USING (memory_id) \
                 WHERE m2.schema_id = m.schema_id \
                   AND m2.owner_principal_kind = m.owner_principal_kind \
                   AND m2.owner_principal_id = m.owner_principal_id \
                   AND m2.owner_org_id = m.owner_org_id \
                   AND s2.repo_id = s.repo_id \
                   AND s2.file_path = s.file_path \
                   AND s2.chunk_index = s.chunk_index \
                   AND m2.created_at > m.created_at \
           )",
    )
    .bind(kind)
    .bind(principal_id)
    .bind(org_id)
    .bind(repo_id)
    .bind(file_path)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|(i,)| u32::try_from(i).unwrap_or(0))
        .collect())
}

/// Look up the current Present `code-chunk-v1` head at
/// `(owner, repo_id, file_path, chunk_index)` and return its
/// `memory_id` iff its stored `text` matches `text_to_match`.
///
/// Used by `LocalGitSource` to skip re-emission of a chunk Fact when
/// the same logical chunk re-appears at a later commit with the same
/// content but at a shifted file byte range. The substrate's
/// content-derived `event_id` would otherwise mint a fresh
/// `memory_id` (the chunk Fact payload includes byte/line ranges,
/// which shift even when the text is identical), causing typed
/// `code/calls` edges to fan out one-per-commit instead of
/// one-per-call-site. Reusing the existing memory_id keeps both the
/// chunk-Fact set and the edge set stable across commits.
///
/// Position-drift trade-off: when a chunk's text is unchanged but
/// it's now at a different `chunk_index` (something added above),
/// the lookup at the new index misses → a fresh Fact is emitted.
/// That's correct: chunk_index is part of the NK and `index drift`
/// is a real change to the chunk's identity within the file.
pub async fn lookup_present_chunk_memory_id_by_text(
    pool: &PgPool,
    owner: &Owner,
    repo_id: uuid::Uuid,
    file_path: &str,
    chunk_index: u32,
    text_to_match: &str,
) -> Result<Option<MemoryId>, IngestError> {
    use proxima_core::Principal;
    let (kind, principal_id) = match &owner.principal {
        Principal::User(u) => ("User", u.into_inner()),
        Principal::Group(g) => ("Group", g.into_inner()),
    };
    let org_id = owner.org_id.into_inner();

    let row: Option<(uuid::Uuid,)> = sqlx::query_as(
        "SELECT m.memory_id \
         FROM proxima_core.memories m \
         JOIN proxima_code.code_chunk_v1 s USING (memory_id) \
         WHERE m.owner_principal_kind = $1 \
           AND m.owner_principal_id = $2 \
           AND m.owner_org_id = $3 \
           AND s.repo_id = $4 \
           AND s.file_path = $5 \
           AND s.chunk_index = $6 \
           AND s.state = 'Present' \
           AND s.text = $7 \
           AND NOT EXISTS ( \
                 SELECT 1 FROM proxima_core.memories m2 \
                 JOIN proxima_code.code_chunk_v1 s2 USING (memory_id) \
                 WHERE m2.schema_id = m.schema_id \
                   AND m2.owner_principal_kind = m.owner_principal_kind \
                   AND m2.owner_principal_id = m.owner_principal_id \
                   AND m2.owner_org_id = m.owner_org_id \
                   AND s2.repo_id = s.repo_id \
                   AND s2.file_path = s.file_path \
                   AND s2.chunk_index = s.chunk_index \
                   AND m2.created_at > m.created_at \
           ) \
         LIMIT 1",
    )
    .bind(kind)
    .bind(principal_id)
    .bind(org_id)
    .bind(repo_id)
    .bind(file_path)
    .bind(i32::try_from(chunk_index).unwrap_or(i32::MAX))
    .bind(text_to_match)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|(mid,)| MemoryId::new(mid)))
}

/// Typed payload for one `code/calls` edge: caller + callee chunk
/// memories, callsite byte range, and callee identifier metadata.
///
/// `callsite_byte_start_in_source_chunk` is the offset of the call
/// expression *within the source chunk's text* (not file-level). It
/// participates in the deterministic `edge_id` derivation so the same
/// caller→callee call site collapses to a single edge across commits
/// where the chunk's content is unchanged but its file-level byte
/// position has shifted (a sibling above changed). The file-level
/// `callsite_byte_start` / `callsite_byte_end` are stored in the
/// sidecar for first-observation context but do not contribute to
/// edge identity.
#[derive(Debug, Clone)]
pub struct CallEdgeDraft {
    pub source_memory_id: uuid::Uuid,
    pub target_memory_id: uuid::Uuid,
    pub callsite_byte_start: u32,
    pub callsite_byte_end: u32,
    pub callsite_byte_start_in_source_chunk: u32,
    pub callee_name: String,
    pub is_dynamic: bool,
}

/// Stable namespace UUID for deterministic `proxima-code` edge_ids.
/// Combined with the natural-key bytes via `Uuid::new_v5`, this
/// produces an `edge_id` that's identical across re-ingests of the
/// same call site — the substrate's `ON CONFLICT (edge_id) DO
/// NOTHING` then drops the duplicate without firing a duplicate
/// `EdgeAppend` change_event.
const PROXIMA_CODE_EDGE_NAMESPACE: uuid::Uuid = uuid::Uuid::from_bytes([
    0xb8, 0xe7, 0xf8, 0xd2, 0x7c, 0x4f, 0x4f, 0x5a, 0x9e, 0x3a, 0x4d, 0x2b, 0x1e, 0x9f, 0x0a, 0x3c,
]);

/// Derive the natural-key bytes for a `proxima-code/calls` edge.
/// Components: owner principal kind / id / org-id, the relation
/// string, both endpoint memory ids, and the **chunk-relative**
/// callsite byte start. File-level offsets are deliberately omitted
/// so the key is stable when chunk content is stable but the chunk
/// has shifted in the file.
fn calls_edge_natural_key(
    owner: &Owner,
    source_memory_id: uuid::Uuid,
    target_memory_id: uuid::Uuid,
    callsite_byte_start_in_source_chunk: u32,
) -> Vec<u8> {
    use proxima_core::Principal;
    let mut k = Vec::with_capacity(128);
    let (kind, principal_id) = match &owner.principal {
        Principal::User(u) => ("User", u.into_inner()),
        Principal::Group(g) => ("Group", g.into_inner()),
    };
    k.extend_from_slice(kind.as_bytes());
    k.push(0);
    k.extend_from_slice(principal_id.as_bytes());
    k.push(0);
    k.extend_from_slice(owner.org_id.into_inner().as_bytes());
    k.push(0);
    k.extend_from_slice(b"proxima-code/calls");
    k.push(0);
    k.extend_from_slice(source_memory_id.as_bytes());
    k.push(0);
    k.extend_from_slice(target_memory_id.as_bytes());
    k.push(0);
    k.extend_from_slice(&callsite_byte_start_in_source_chunk.to_be_bytes());
    k
}

/// Atomic edge + typed sidecar write for `code/calls` edges.
/// Wraps `proxima_storage_pg::verbs::edge_append::append_edge_in_tx`.
///
/// `edge_id` is derived deterministically from the natural key
/// (owner ‖ relation ‖ source_memory_id ‖ target_memory_id ‖
/// chunk-relative callsite offset). Re-ingests of the same call site
/// produce the same `edge_id` and are dropped by the `ON CONFLICT`
/// guard inside `append_edge_in_tx` — sidecar + change_event are
/// gated on the edge insert actually returning a row.
pub async fn ingest_calls_edge(
    pool: &PgPool,
    owner: &Owner,
    edge: &CallEdgeDraft,
) -> Result<(), IngestError> {
    use proxima_storage_pg::verbs::edge_append::{EdgeDraft, append_edge_in_tx};

    let registry = schema_registry();
    let relation = registry
        .resolve_relation("proxima-code/calls")
        .ok_or_else(|| {
            IngestError::Storage("missing registered relation proxima-code/calls".into())
        })?;

    let key = calls_edge_natural_key(
        owner,
        edge.source_memory_id,
        edge.target_memory_id,
        edge.callsite_byte_start_in_source_chunk,
    );
    let edge_id = uuid::Uuid::new_v5(&PROXIMA_CODE_EDGE_NAMESPACE, &key);

    let payload = serde_json::json!({
        "callsite_byte_start": edge.callsite_byte_start,
        "callsite_byte_end": edge.callsite_byte_end,
        "callee_name": edge.callee_name,
        "is_dynamic": edge.is_dynamic,
    });

    let draft = EdgeDraft {
        edge_id,
        relation,
        source_kind: "Fact",
        source_memory_id: Some(edge.source_memory_id),
        source_goal_id: None,
        target_kind: "Fact",
        target_memory_id: Some(edge.target_memory_id),
        target_goal_id: None,
        authorship_kind: "EventSource",
        authorship_owner_memory_id: Some(edge.source_memory_id),
        owner,
    };

    let mut tx = pool.begin().await?;
    append_edge_in_tx(&mut tx, &draft, Some(&payload)).await?;
    tx.commit().await?;

    Ok(())
}

#[must_use]
pub fn schema_registry() -> proxima_core::verbs::schema::SchemaRegistry {
    use proxima_core::verbs::schema::{PayloadKind, SchemaInfo, SchemaRegistry};
    use proxima_core::{FlavorRegistry, SchemaId, SchemaVersion};

    let mut flavor = FlavorRegistry::new();
    crate::register(&mut flavor);
    let flavor = flavor.freeze();
    let mut schemas = flavor.list();
    let relations = flavor.list_relations().to_vec();

    // CitedObject schemas — file blob (shared by file_revision + chunk)
    // and commit object.
    for cited in [CODE_BLOB_SCHEMA, CODE_COMMIT_OBJECT_SCHEMA] {
        schemas.push(SchemaInfo {
            schema_id: SchemaId::new(cited.into()),
            schema_version: SchemaVersion::new(1),
            kind: PayloadKind::CitedObject,
            filter_keys: vec![],
            sidecar_table: None,
            natural_key_columns: vec![],
        });
    }

    // CitationMapping schemas — typed per fact-type.
    for mapping in [
        CODE_BLOB_WHOLE_SCHEMA,
        CODE_BLOB_BYTE_RANGE_SCHEMA,
        CODE_COMMIT_WHOLE_SCHEMA,
    ] {
        schemas.push(SchemaInfo {
            schema_id: SchemaId::new(mapping.into()),
            schema_version: SchemaVersion::new(1),
            kind: PayloadKind::CitationMapping,
            filter_keys: vec![],
            sidecar_table: None,
            natural_key_columns: vec![],
        });
    }

    SchemaRegistry::with_schemas_and_relations(schemas, relations)
}

/// Convenience: build a fully-wired `Engine` over a `PgStorage` and the
/// proxima-code flavor's schemas plus the helper-required cited /
/// citation schemas. Used by tests and the composite binary.
#[must_use]
pub fn build_engine(storage: PgStorage, auth: Box<dyn proxima_core::auth::AuthResolver>) -> Engine {
    use proxima_core::verbs::query::MemoryStore;

    Engine::new(schema_registry(), MemoryStore::new(), auth).with_storage(Arc::new(storage))
}

// Suppress dead-code from the convenience exports until the composite
// bin in B.6 starts using them.
#[allow(dead_code)]
fn _unused() -> &'static str {
    let _ = QueryRequest::for_owner;
    let _ = SupersessionStatus::HeadsOnly;
    let _ = Credentials::None;
    "noop"
}
