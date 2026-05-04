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

/// Stable schema-ids for the flavor's helper-required hints. The
/// composite binary is responsible for registering these (or whatever
/// schemas it prefers) in the `SchemaRegistry` so `Engine::event_ingest`
/// validation passes.
pub const CITED_OBJECT_SCHEMA: &str = "proxima-code/cited-object-v1";
pub const CITATION_MAPPING_SCHEMA: &str = "proxima-code/citation-mapping-v1";

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

fn make_draft<P: serde::Serialize>(
    owner: &Owner,
    source_batch_id: SourceBatchId,
    payload: &P,
    schema_id: &str,
    cited_object_hash: [u8; 32],
    observed_at: time::OffsetDateTime,
) -> Result<EventDraft, IngestError> {
    let bytes =
        serde_json::to_vec(payload).map_err(|e| IngestError::Serialize(e.to_string()))?;
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
            schema_id: SchemaId::new(CITED_OBJECT_SCHEMA.into()),
            schema_version: SchemaVersion::new(1),
            content_hash: cited_object_hash,
        },
        citation_mapping: CitationMappingHint {
            schema_id: SchemaId::new(CITATION_MAPPING_SCHEMA.into()),
            schema_version: SchemaVersion::new(1),
        },
    })
}

/// Atomic Fact + sidecar write for `commit-v1`.
pub async fn ingest_commit(
    pool: &PgPool,
    owner: &Owner,
    source_batch_id: SourceBatchId,
    payload: &CommitV1,
    observed_at: time::OffsetDateTime,
) -> Result<EventIngestOutcome, IngestError> {
    let cited_hash = blake3::hash(payload.sha.as_bytes()).into();
    let draft = make_draft(
        owner,
        source_batch_id,
        payload,
        CommitV1::SCHEMA_ID,
        cited_hash,
        observed_at,
    )?;

    let mut tx = pool.begin().await.map_err(|e| IngestError::Storage(e.to_string()))?;
    let outcome = ingest_event_in_tx(&mut tx, &draft)
        .await
        .map_err(|e| IngestError::Storage(e.to_string()))?;
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
        .await
        .map_err(|e| IngestError::Storage(e.to_string()))?;
    }
    tx.commit().await.map_err(|e| IngestError::Storage(e.to_string()))?;
    Ok(outcome)
}

/// Atomic Fact + sidecar write for `file-revision-v1`.
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
        payload.content_sha256,
        observed_at,
    )?;

    let mut tx = pool.begin().await.map_err(|e| IngestError::Storage(e.to_string()))?;
    let outcome = ingest_event_in_tx(&mut tx, &draft)
        .await
        .map_err(|e| IngestError::Storage(e.to_string()))?;
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
        .await
        .map_err(|e| IngestError::Storage(e.to_string()))?;
    }
    tx.commit().await.map_err(|e| IngestError::Storage(e.to_string()))?;
    Ok(outcome)
}

/// Atomic Fact + sidecar write for `code-chunk-v1`.
pub async fn ingest_code_chunk(
    pool: &PgPool,
    owner: &Owner,
    source_batch_id: SourceBatchId,
    payload: &CodeChunkV1,
    observed_at: time::OffsetDateTime,
) -> Result<EventIngestOutcome, IngestError> {
    let cited_hash = blake3::hash(payload.text.as_bytes()).into();
    let draft = make_draft(
        owner,
        source_batch_id,
        payload,
        CodeChunkV1::SCHEMA_ID,
        cited_hash,
        observed_at,
    )?;

    let mut tx = pool.begin().await.map_err(|e| IngestError::Storage(e.to_string()))?;
    let outcome = ingest_event_in_tx(&mut tx, &draft)
        .await
        .map_err(|e| IngestError::Storage(e.to_string()))?;
    if !outcome.idempotent_replay {
        let state_text = match payload.state {
            FileState::Present => "Present",
            FileState::Tombstone => "Tombstone",
        };
        sqlx::query(
            "INSERT INTO proxima_code.code_chunk_v1 \
                (memory_id, repo_id, file_path, chunk_index, parent_file_revision_id, \
                 text, language, chunk_type, byte_range_start, byte_range_end, \
                 line_range_start, line_range_end, state) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)",
        )
        .bind(outcome.memory_id.into_inner())
        .bind(payload.repo_id)
        .bind(&payload.file_path)
        .bind(i32::try_from(payload.chunk_index).unwrap_or(i32::MAX))
        .bind(payload.parent_file_revision_id.into_inner())
        .bind(&payload.text)
        .bind(payload.language.as_deref())
        .bind(&payload.chunk_type)
        .bind(i64::from(payload.byte_range_start))
        .bind(i64::from(payload.byte_range_end))
        .bind(i64::from(payload.line_range_start))
        .bind(i64::from(payload.line_range_end))
        .bind(state_text)
        .execute(&mut *tx)
        .await
        .map_err(|e| IngestError::Storage(e.to_string()))?;
    }
    tx.commit().await.map_err(|e| IngestError::Storage(e.to_string()))?;
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
    .await
    .map_err(|e| IngestError::Storage(e.to_string()))?;

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
    .await
    .map_err(|e| IngestError::Storage(e.to_string()))?;

    Ok(rows
        .into_iter()
        .map(|(i,)| u32::try_from(i).unwrap_or(0))
        .collect())
}

/// Convenience: build a fully-wired `Engine` over a `PgStorage` and the
/// proxima-code flavor's schemas plus the helper-required cited /
/// citation schemas. Used by tests and the composite binary.
#[must_use]
pub fn build_engine(
    storage: PgStorage,
    auth: Box<dyn proxima_core::auth::AuthResolver>,
) -> Engine {
    use proxima_core::verbs::query::MemoryStore;
    use proxima_core::verbs::schema::{PayloadKind, SchemaInfo, SchemaRegistry};
    use proxima_core::{FlavorRegistry, SchemaId, SchemaVersion};

    let mut flavor = FlavorRegistry::new();
    crate::register(&mut flavor);
    let mut schemas = flavor.freeze().list();
    schemas.push(SchemaInfo {
        schema_id: SchemaId::new(CITED_OBJECT_SCHEMA.into()),
        schema_version: SchemaVersion::new(1),
        kind: PayloadKind::CitedObject,
        filter_keys: vec![],
        sidecar_table: None,
        natural_key_columns: vec![],
    });
    schemas.push(SchemaInfo {
        schema_id: SchemaId::new(CITATION_MAPPING_SCHEMA.into()),
        schema_version: SchemaVersion::new(1),
        kind: PayloadKind::CitationMapping,
        filter_keys: vec![],
        sidecar_table: None,
        natural_key_columns: vec![],
    });

    Engine::new(SchemaRegistry::with_schemas(schemas), MemoryStore::new(), auth)
        .with_storage(Arc::new(storage))
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
