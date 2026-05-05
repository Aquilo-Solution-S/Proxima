//! Repo registry — tracks local-path git repos registered by the user
//! for ingestion. One row per (`Owner`, `canonical_path`). The cursor
//! advances on each successful `run_poll`, persisted via `update_cursor`.

use proxima_core::{Owner, Principal};
use sqlx::PgPool;
use std::str::FromStr;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct RepoRecord {
    pub repo_id: Uuid,
    pub canonical_path: String,
    pub display_name: String,
    pub last_cursor: Option<Vec<u8>>,
    pub last_polled_at: Option<time::OffsetDateTime>,
    pub created_at: time::OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct RepoEraseReceipt {
    pub repo_id: Uuid,
    pub completed_at: time::OffsetDateTime,
    pub facts_deleted: u64,
    pub abstractions_deleted: u64,
    pub edges_deleted: u64,
    pub embeddings_deleted: u64,
    pub events_deleted: u64,
    pub citation_mappings_deleted: u64,
    pub cited_objects_deleted: u64,
    pub source_batches_deleted: u64,
    pub f2a_rows_deleted: u64,
    pub repo_record_deleted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
}

impl RunStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
        }
    }
}

impl FromStr for RunStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "queued" => Ok(Self::Queued),
            "running" => Ok(Self::Running),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            other => Err(format!("unknown run status: {other}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
#[serde(rename_all = "snake_case")]
pub enum RunStage {
    Starting,
    Facts,
    AstEdges,
    F2a,
    Embeddings,
    Done,
}

impl RunStage {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Facts => "facts",
            Self::AstEdges => "ast_edges",
            Self::F2a => "f2a",
            Self::Embeddings => "embeddings",
            Self::Done => "done",
        }
    }
}

impl FromStr for RunStage {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "starting" => Ok(Self::Starting),
            "facts" => Ok(Self::Facts),
            "ast_edges" => Ok(Self::AstEdges),
            "f2a" => Ok(Self::F2a),
            "embeddings" => Ok(Self::Embeddings),
            "done" => Ok(Self::Done),
            other => Err(format!("unknown run stage: {other}")),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, specta::Type)]
pub struct RepoIngestionRun {
    pub run_id: Uuid,
    pub repo_id: Uuid,
    pub status: RunStatus,
    pub stage: RunStage,
    pub commits_emitted: u32,
    pub files_emitted: u32,
    pub chunks_emitted: u32,
    pub chunks_reused: u32,
    pub chunks_tombstoned: u32,
    pub ast_edges_emitted: u32,
    pub abstractions_emitted: u32,
    pub embeddings_landed: u32,
    pub citations_emitted: u32,
    pub error_message: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub started_at: time::OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: time::OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub finished_at: Option<time::OffsetDateTime>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct StageCounters {
    pub commits_emitted: u32,
    pub files_emitted: u32,
    pub chunks_emitted: u32,
    pub chunks_reused: u32,
    pub chunks_tombstoned: u32,
    pub ast_edges_emitted: u32,
    pub abstractions_emitted: u32,
    pub embeddings_landed: u32,
    pub citations_emitted: u32,
}

impl StageCounters {
    #[must_use]
    pub const fn zeroed() -> Self {
        Self {
            commits_emitted: 0,
            files_emitted: 0,
            chunks_emitted: 0,
            chunks_reused: 0,
            chunks_tombstoned: 0,
            ast_edges_emitted: 0,
            abstractions_emitted: 0,
            embeddings_landed: 0,
            citations_emitted: 0,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RepoRegistryError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("duplicate repo at path: {canonical_path}")]
    DuplicatePath { canonical_path: String },
    #[error("repo not found: {repo_id}")]
    NotFound { repo_id: Uuid },
    #[error("ingestion run not found: {run_id}")]
    RunNotFound { run_id: Uuid },
    #[error("ingestion run is already in terminal state: {run_id} ({status:?})")]
    RunAlreadyTerminal { run_id: Uuid, status: RunStatus },
}

/// Encode `Owner` into the three column values used by the `repos` table.
fn owner_columns(owner: &Owner) -> (&'static str, uuid::Uuid, uuid::Uuid) {
    let (kind, principal_id) = match &owner.principal {
        Principal::User(u) => ("User", u.into_inner()),
        Principal::Group(g) => ("Group", g.into_inner()),
    };
    (kind, principal_id, owner.org_id.into_inner())
}

#[doc(hidden)]
#[must_use]
pub fn owner_columns_pub(owner: &Owner) -> (&'static str, uuid::Uuid, uuid::Uuid) {
    owner_columns(owner)
}

/// List all repos registered for `owner`, oldest first.
///
/// # Errors
/// Returns `RepoRegistryError::Database` on database failures.
pub async fn list_repos(
    pool: &PgPool,
    owner: &Owner,
) -> Result<Vec<RepoRecord>, RepoRegistryError> {
    let (kind, principal_id, org_id) = owner_columns(owner);

    let rows = sqlx::query_as::<_, RepoRow>(
        "SELECT repo_id, canonical_path, display_name, last_cursor, last_polled_at, created_at \
         FROM proxima_code.repos \
         WHERE owner_principal_kind = $1 \
           AND owner_principal_id = $2 \
           AND owner_org_id = $3 \
         ORDER BY created_at ASC",
    )
    .bind(kind)
    .bind(principal_id)
    .bind(org_id)
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().map(Into::into).collect())
}

/// Caller pre-canonicalizes the path. On unique-violation, returns
/// `RepoRegistryError::DuplicatePath`.
///
/// # Errors
/// `RepoRegistryError::DuplicatePath` if the path is already registered
/// for this owner; `RepoRegistryError::Database` on database failures.
pub async fn register_repo(
    pool: &PgPool,
    owner: &Owner,
    repo_id: Uuid,
    canonical_path: &str,
    display_name: &str,
) -> Result<RepoRecord, RepoRegistryError> {
    let (kind, principal_id, org_id) = owner_columns(owner);

    let row = sqlx::query_as::<_, RepoRow>(
        "INSERT INTO proxima_code.repos \
            (owner_principal_kind, owner_principal_id, owner_org_id, \
             repo_id, canonical_path, display_name, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, now()) \
         ON CONFLICT (owner_principal_kind, owner_principal_id, owner_org_id, canonical_path) \
         DO NOTHING \
         RETURNING repo_id, canonical_path, display_name, last_cursor, last_polled_at, created_at",
    )
    .bind(kind)
    .bind(principal_id)
    .bind(org_id)
    .bind(repo_id)
    .bind(canonical_path)
    .bind(display_name)
    .fetch_optional(pool)
    .await?;

    if let Some(r) = row {
        return Ok(r.into());
    }
    // ON CONFLICT DO NOTHING ate the insert. Either the path is already
    // registered (the expected case) or something raced; verify which.
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS( \
             SELECT 1 FROM proxima_code.repos \
             WHERE owner_principal_kind = $1 \
               AND owner_principal_id = $2 \
               AND owner_org_id = $3 \
               AND canonical_path = $4 \
         )",
    )
    .bind(kind)
    .bind(principal_id)
    .bind(org_id)
    .bind(canonical_path)
    .fetch_one(pool)
    .await?;
    if exists {
        Err(RepoRegistryError::DuplicatePath {
            canonical_path: canonical_path.to_string(),
        })
    } else {
        Err(RepoRegistryError::NotFound { repo_id })
    }
}

/// Delete the repo record for `(owner, repo_id)`. Returns `true` if a row
/// was deleted, `false` if no matching row existed.
///
/// # Errors
/// Returns `RepoRegistryError::Database` on database failures.
pub async fn delete_repo(
    pool: &PgPool,
    owner: &Owner,
    repo_id: Uuid,
) -> Result<bool, RepoRegistryError> {
    let (kind, principal_id, org_id) = owner_columns(owner);

    let result = sqlx::query(
        "DELETE FROM proxima_code.repos \
         WHERE owner_principal_kind = $1 \
           AND owner_principal_id = $2 \
           AND owner_org_id = $3 \
           AND repo_id = $4",
    )
    .bind(kind)
    .bind(principal_id)
    .bind(org_id)
    .bind(repo_id)
    .execute(pool)
    .await?;

    Ok(result.rows_affected() > 0)
}

/// Hard-delete one repo's code-flavor data for a clear reingestion.
///
/// This is intentionally explicit rather than FK-cascade based: cited
/// objects and source batches are substrate rows and are deleted only
/// when no remaining rows reference them after the repo-scoped data is
/// removed.
///
/// # Errors
/// Returns `RepoRegistryError::NotFound` if the repo is not registered
/// for `owner`; `RepoRegistryError::Database` on database failures.
#[allow(clippy::too_many_lines)]
pub async fn erase_repo(
    pool: &PgPool,
    owner: &Owner,
    repo_id: Uuid,
) -> Result<RepoEraseReceipt, RepoRegistryError> {
    let (kind, principal_id, org_id) = owner_columns(owner);
    let mut tx = pool.begin().await?;

    let exists: Option<(Uuid,)> = sqlx::query_as(
        "SELECT repo_id \
         FROM proxima_code.repos \
         WHERE owner_principal_kind = $1 \
           AND owner_principal_id = $2 \
           AND owner_org_id = $3 \
           AND repo_id = $4 \
         FOR UPDATE",
    )
    .bind(kind)
    .bind(principal_id)
    .bind(org_id)
    .bind(repo_id)
    .fetch_optional(&mut *tx)
    .await?;
    if exists.is_none() {
        tx.rollback().await?;
        return Err(RepoRegistryError::NotFound { repo_id });
    }

    sqlx::query("SET CONSTRAINTS ALL DEFERRED")
        .execute(&mut *tx)
        .await?;

    sqlx::query(
        "CREATE TEMP TABLE tmp_proxima_repo_facts \
            (memory_id uuid PRIMARY KEY) ON COMMIT DROP",
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO tmp_proxima_repo_facts (memory_id) \
         SELECT m.memory_id \
         FROM proxima_core.memories m \
         JOIN proxima_code.commit_v1 s USING (memory_id) \
         WHERE m.owner_principal_kind = $1 \
           AND m.owner_principal_id = $2 \
           AND m.owner_org_id = $3 \
           AND s.repo_id = $4 \
         UNION \
         SELECT m.memory_id \
         FROM proxima_core.memories m \
         JOIN proxima_code.file_revision_v1 s USING (memory_id) \
         WHERE m.owner_principal_kind = $1 \
           AND m.owner_principal_id = $2 \
           AND m.owner_org_id = $3 \
           AND s.repo_id = $4 \
         UNION \
         SELECT m.memory_id \
         FROM proxima_core.memories m \
         JOIN proxima_code.code_chunk_v1 s USING (memory_id) \
         WHERE m.owner_principal_kind = $1 \
           AND m.owner_principal_id = $2 \
           AND m.owner_org_id = $3 \
           AND s.repo_id = $4",
    )
    .bind(kind)
    .bind(principal_id)
    .bind(org_id)
    .bind(repo_id)
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        "CREATE TEMP TABLE tmp_proxima_repo_abstractions \
            (memory_id uuid PRIMARY KEY) ON COMMIT DROP",
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO tmp_proxima_repo_abstractions (memory_id) \
         SELECT m.memory_id \
         FROM proxima_core.memories m \
         JOIN proxima_code.commit_summary_v1 s USING (memory_id) \
         WHERE m.owner_principal_kind = $1 \
           AND m.owner_principal_id = $2 \
           AND m.owner_org_id = $3 \
           AND s.repo_id = $4",
    )
    .bind(kind)
    .bind(principal_id)
    .bind(org_id)
    .bind(repo_id)
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        "CREATE TEMP TABLE tmp_proxima_repo_memories \
            (memory_id uuid PRIMARY KEY) ON COMMIT DROP",
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO tmp_proxima_repo_memories (memory_id) \
         SELECT memory_id FROM tmp_proxima_repo_facts \
         UNION \
         SELECT memory_id FROM tmp_proxima_repo_abstractions",
    )
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        "CREATE TEMP TABLE tmp_proxima_repo_events \
            (event_id bytea PRIMARY KEY, source_batch_id uuid NOT NULL) ON COMMIT DROP",
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO tmp_proxima_repo_events (event_id, source_batch_id) \
         SELECT e.event_id, e.source_batch_id \
         FROM proxima_core.events e \
         JOIN proxima_core.memories m ON m.event_id = e.event_id \
         JOIN tmp_proxima_repo_facts f ON f.memory_id = m.memory_id",
    )
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        "CREATE TEMP TABLE tmp_proxima_repo_batches \
            (batch_id uuid PRIMARY KEY) ON COMMIT DROP",
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO tmp_proxima_repo_batches (batch_id) \
         SELECT DISTINCT source_batch_id FROM tmp_proxima_repo_events",
    )
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        "CREATE TEMP TABLE tmp_proxima_repo_citation_mappings \
            (citation_mapping_id uuid PRIMARY KEY, cited_object_id uuid NOT NULL) ON COMMIT DROP",
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO tmp_proxima_repo_citation_mappings \
            (citation_mapping_id, cited_object_id) \
         SELECT cm.citation_mapping_id, cm.cited_object_id \
         FROM proxima_core.citation_mappings cm \
         JOIN tmp_proxima_repo_facts f ON f.memory_id = cm.memory_id",
    )
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        "CREATE TEMP TABLE tmp_proxima_repo_cited_objects \
            (cited_object_id uuid PRIMARY KEY) ON COMMIT DROP",
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO tmp_proxima_repo_cited_objects (cited_object_id) \
         SELECT DISTINCT cited_object_id FROM tmp_proxima_repo_citation_mappings",
    )
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        "CREATE TEMP TABLE tmp_proxima_repo_edges \
            (edge_id uuid PRIMARY KEY) ON COMMIT DROP",
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO tmp_proxima_repo_edges (edge_id) \
         SELECT e.edge_id \
         FROM proxima_core.edges e \
         WHERE e.owner_principal_kind = $1 \
           AND e.owner_principal_id = $2 \
           AND e.owner_org_id = $3 \
           AND ( \
                e.source_memory_id IN (SELECT memory_id FROM tmp_proxima_repo_memories) \
             OR e.target_memory_id IN (SELECT memory_id FROM tmp_proxima_repo_memories) \
             OR e.authorship_owner_memory_id IN (SELECT memory_id FROM tmp_proxima_repo_memories) \
           )",
    )
    .bind(kind)
    .bind(principal_id)
    .bind(org_id)
    .execute(&mut *tx)
    .await?;

    let mut receipt = RepoEraseReceipt {
        repo_id,
        completed_at: time::OffsetDateTime::now_utc(),
        facts_deleted: 0,
        abstractions_deleted: 0,
        edges_deleted: 0,
        embeddings_deleted: 0,
        events_deleted: 0,
        citation_mappings_deleted: 0,
        cited_objects_deleted: 0,
        source_batches_deleted: 0,
        f2a_rows_deleted: 0,
        repo_record_deleted: false,
    };

    receipt.facts_deleted = count_temp_rows(&mut tx, "tmp_proxima_repo_facts").await?;
    receipt.abstractions_deleted =
        count_temp_rows(&mut tx, "tmp_proxima_repo_abstractions").await?;

    sqlx::query(
        "DELETE FROM proxima_core.change_event \
         WHERE entity_memory_id IN (SELECT memory_id FROM tmp_proxima_repo_memories) \
            OR edge_id IN (SELECT edge_id FROM tmp_proxima_repo_edges)",
    )
    .execute(&mut *tx)
    .await?;

    receipt.embeddings_deleted = sqlx::query(
        "DELETE FROM proxima_core.embeddings \
         WHERE entity_id IN (SELECT memory_id FROM tmp_proxima_repo_memories)",
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();

    sqlx::query(
        "DELETE FROM proxima_code.code_calls_v1 \
         WHERE edge_id IN (SELECT edge_id FROM tmp_proxima_repo_edges)",
    )
    .execute(&mut *tx)
    .await?;

    receipt.f2a_rows_deleted = sqlx::query(
        "DELETE FROM proxima_core.source_batch_f2a \
         WHERE batch_id IN (SELECT batch_id FROM tmp_proxima_repo_batches) \
            OR head_memory_id IN (SELECT memory_id FROM tmp_proxima_repo_memories)",
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();

    receipt.edges_deleted = sqlx::query(
        "DELETE FROM proxima_core.edges \
         WHERE edge_id IN (SELECT edge_id FROM tmp_proxima_repo_edges)",
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();

    sqlx::query(
        "DELETE FROM proxima_code.commit_summary_v1 \
         WHERE memory_id IN (SELECT memory_id FROM tmp_proxima_repo_abstractions)",
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "DELETE FROM proxima_code.code_chunk_v1 \
         WHERE memory_id IN (SELECT memory_id FROM tmp_proxima_repo_facts)",
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "DELETE FROM proxima_code.file_revision_v1 \
         WHERE memory_id IN (SELECT memory_id FROM tmp_proxima_repo_facts)",
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "DELETE FROM proxima_code.commit_v1 \
         WHERE memory_id IN (SELECT memory_id FROM tmp_proxima_repo_facts)",
    )
    .execute(&mut *tx)
    .await?;

    receipt.citation_mappings_deleted = sqlx::query(
        "DELETE FROM proxima_core.citation_mappings \
         WHERE citation_mapping_id IN ( \
             SELECT citation_mapping_id FROM tmp_proxima_repo_citation_mappings \
         )",
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();

    sqlx::query(
        "DELETE FROM proxima_core.memories \
         WHERE memory_id IN (SELECT memory_id FROM tmp_proxima_repo_memories)",
    )
    .execute(&mut *tx)
    .await?;

    receipt.events_deleted = sqlx::query(
        "DELETE FROM proxima_core.events \
         WHERE event_id IN (SELECT event_id FROM tmp_proxima_repo_events)",
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();

    receipt.source_batches_deleted = sqlx::query(
        "DELETE FROM proxima_core.source_batches sb \
         WHERE sb.id IN (SELECT batch_id FROM tmp_proxima_repo_batches) \
           AND NOT EXISTS ( \
               SELECT 1 FROM proxima_core.events e WHERE e.source_batch_id = sb.id \
           )",
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();

    receipt.cited_objects_deleted = sqlx::query(
        "DELETE FROM proxima_core.cited_objects co \
         WHERE co.cited_object_id IN ( \
             SELECT cited_object_id FROM tmp_proxima_repo_cited_objects \
         ) \
           AND NOT EXISTS ( \
               SELECT 1 FROM proxima_core.citation_mappings cm \
               WHERE cm.cited_object_id = co.cited_object_id \
           )",
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();

    receipt.repo_record_deleted = sqlx::query(
        "DELETE FROM proxima_code.repos \
         WHERE owner_principal_kind = $1 \
           AND owner_principal_id = $2 \
           AND owner_org_id = $3 \
           AND repo_id = $4",
    )
    .bind(kind)
    .bind(principal_id)
    .bind(org_id)
    .bind(repo_id)
    .execute(&mut *tx)
    .await?
    .rows_affected()
        > 0;

    tx.commit().await?;
    Ok(receipt)
}

async fn count_temp_rows(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    table: &str,
) -> Result<u64, sqlx::Error> {
    let sql = format!("SELECT COUNT(*)::bigint FROM {table}");
    let count: i64 = sqlx::query_scalar(&sql).fetch_one(&mut **tx).await?;
    Ok(u64::try_from(count).unwrap_or(0))
}

/// Look up a single repo record by `(owner, repo_id)`.
///
/// # Errors
/// Returns `RepoRegistryError::Database` on database failures.
pub async fn get_repo(
    pool: &PgPool,
    owner: &Owner,
    repo_id: Uuid,
) -> Result<Option<RepoRecord>, RepoRegistryError> {
    let (kind, principal_id, org_id) = owner_columns(owner);

    let row = sqlx::query_as::<_, RepoRow>(
        "SELECT repo_id, canonical_path, display_name, last_cursor, last_polled_at, created_at \
         FROM proxima_code.repos \
         WHERE owner_principal_kind = $1 \
           AND owner_principal_id = $2 \
           AND owner_org_id = $3 \
           AND repo_id = $4",
    )
    .bind(kind)
    .bind(principal_id)
    .bind(org_id)
    .bind(repo_id)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(Into::into))
}

/// Persist new `cursor` + `polled_at` after a successful `run_poll`.
///
/// # Errors
/// Returns `RepoRegistryError::Database` on database failures.
pub async fn update_cursor(
    pool: &PgPool,
    owner: &Owner,
    repo_id: Uuid,
    cursor_bytes: &[u8],
    polled_at: time::OffsetDateTime,
) -> Result<(), RepoRegistryError> {
    let (kind, principal_id, org_id) = owner_columns(owner);

    sqlx::query(
        "UPDATE proxima_code.repos \
         SET last_cursor = $4, last_polled_at = $5 \
         WHERE owner_principal_kind = $1 \
           AND owner_principal_id = $2 \
           AND owner_org_id = $3 \
           AND repo_id = $6",
    )
    .bind(kind)
    .bind(principal_id)
    .bind(org_id)
    .bind(cursor_bytes)
    .bind(polled_at)
    .bind(repo_id)
    .execute(pool)
    .await?;

    Ok(())
}

/// Create a queued run or return the active row for `(owner, repo_id)`.
///
/// # Errors
/// Returns `RepoRegistryError::Database` on database failures.
pub async fn start_run(
    pool: &PgPool,
    owner: &Owner,
    repo_id: Uuid,
) -> Result<RepoIngestionRun, RepoRegistryError> {
    let (run, _) = start_run_with_created(pool, owner, repo_id).await?;
    Ok(run)
}

/// Create a queued run or return the active row plus whether this call inserted.
///
/// # Errors
/// Returns `RepoRegistryError::Database` on database failures.
pub async fn start_run_with_created(
    pool: &PgPool,
    owner: &Owner,
    repo_id: Uuid,
) -> Result<(RepoIngestionRun, bool), RepoRegistryError> {
    let (kind, principal_id, org_id) = owner_columns(owner);
    let new_run_id = Uuid::now_v7();

    let inserted = sqlx::query_as::<_, RunRow>(
        "INSERT INTO proxima_code.repo_ingestion_runs \
            (run_id, owner_principal_kind, owner_principal_id, owner_org_id, \
             repo_id, status, stage) \
         VALUES ($1, $2, $3, $4, $5, 'queued', 'starting') \
         ON CONFLICT (owner_principal_kind, owner_principal_id, owner_org_id, repo_id) \
             WHERE status IN ('queued', 'running') \
         DO NOTHING \
         RETURNING run_id, repo_id, status, stage, \
                   commits_emitted, files_emitted, chunks_emitted, chunks_reused, \
                   chunks_tombstoned, ast_edges_emitted, abstractions_emitted, \
                   embeddings_landed, citations_emitted, \
                   error_message, started_at, updated_at, finished_at",
    )
    .bind(new_run_id)
    .bind(kind)
    .bind(principal_id)
    .bind(org_id)
    .bind(repo_id)
    .fetch_optional(pool)
    .await?;

    if let Some(row) = inserted {
        return Ok((row.into(), true));
    }

    let run = get_active_run(pool, owner, repo_id)
        .await?
        .ok_or(RepoRegistryError::NotFound { repo_id })?;
    Ok((run, false))
}

/// Return the active queued/running run for `(owner, repo_id)`.
///
/// # Errors
/// Returns `RepoRegistryError::Database` on database failures.
pub async fn get_active_run(
    pool: &PgPool,
    owner: &Owner,
    repo_id: Uuid,
) -> Result<Option<RepoIngestionRun>, RepoRegistryError> {
    let (kind, principal_id, org_id) = owner_columns(owner);
    let row = sqlx::query_as::<_, RunRow>(
        "SELECT run_id, repo_id, status, stage, \
                commits_emitted, files_emitted, chunks_emitted, chunks_reused, \
                chunks_tombstoned, ast_edges_emitted, abstractions_emitted, \
                embeddings_landed, citations_emitted, \
                error_message, started_at, updated_at, finished_at \
         FROM proxima_code.repo_ingestion_runs \
         WHERE owner_principal_kind = $1 AND owner_principal_id = $2 \
           AND owner_org_id = $3 AND repo_id = $4 \
           AND status IN ('queued', 'running') \
         ORDER BY started_at DESC \
         LIMIT 1",
    )
    .bind(kind)
    .bind(principal_id)
    .bind(org_id)
    .bind(repo_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(Into::into))
}

/// Return one ingestion run by id.
///
/// # Errors
/// Returns `RepoRegistryError::Database` on database failures.
pub async fn get_run(
    pool: &PgPool,
    run_id: Uuid,
) -> Result<Option<RepoIngestionRun>, RepoRegistryError> {
    let row = sqlx::query_as::<_, RunRow>(
        "SELECT run_id, repo_id, status, stage, \
                commits_emitted, files_emitted, chunks_emitted, chunks_reused, \
                chunks_tombstoned, ast_edges_emitted, abstractions_emitted, \
                embeddings_landed, citations_emitted, \
                error_message, started_at, updated_at, finished_at \
         FROM proxima_code.repo_ingestion_runs \
         WHERE run_id = $1",
    )
    .bind(run_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(Into::into))
}

/// Persist a stage boundary snapshot and return the updated row.
///
/// # Errors
/// Returns `RunNotFound`, `RunAlreadyTerminal`, or database errors.
pub async fn advance_stage(
    pool: &PgPool,
    run_id: Uuid,
    next_stage: RunStage,
    counters: &StageCounters,
) -> Result<RepoIngestionRun, RepoRegistryError> {
    let row = sqlx::query_as::<_, RunRow>(
        "UPDATE proxima_code.repo_ingestion_runs SET \
            status = 'running', stage = $2, \
            commits_emitted = $3, files_emitted = $4, chunks_emitted = $5, \
            chunks_reused = $6, chunks_tombstoned = $7, ast_edges_emitted = $8, \
            abstractions_emitted = $9, embeddings_landed = $10, citations_emitted = $11, \
            updated_at = now() \
          WHERE run_id = $1 AND status NOT IN ('succeeded', 'failed') \
          RETURNING run_id, repo_id, status, stage, \
                    commits_emitted, files_emitted, chunks_emitted, chunks_reused, \
                    chunks_tombstoned, ast_edges_emitted, abstractions_emitted, \
                    embeddings_landed, citations_emitted, \
                    error_message, started_at, updated_at, finished_at",
    )
    .bind(run_id)
    .bind(next_stage.as_str())
    .bind(i32_from_u32(counters.commits_emitted))
    .bind(i32_from_u32(counters.files_emitted))
    .bind(i32_from_u32(counters.chunks_emitted))
    .bind(i32_from_u32(counters.chunks_reused))
    .bind(i32_from_u32(counters.chunks_tombstoned))
    .bind(i32_from_u32(counters.ast_edges_emitted))
    .bind(i32_from_u32(counters.abstractions_emitted))
    .bind(i32_from_u32(counters.embeddings_landed))
    .bind(i32_from_u32(counters.citations_emitted))
    .fetch_optional(pool)
    .await?;

    if let Some(row) = row {
        Ok(row.into())
    } else {
        terminal_or_not_found(pool, run_id).await
    }
}

/// Atomically claim a queued run for the background driver.
///
/// Returns `None` when another driver already claimed the row.
///
/// # Errors
/// Returns `RepoRegistryError::Database` on database failures.
pub async fn begin_run(
    pool: &PgPool,
    run_id: Uuid,
) -> Result<Option<RepoIngestionRun>, RepoRegistryError> {
    let row = sqlx::query_as::<_, RunRow>(
        "UPDATE proxima_code.repo_ingestion_runs SET \
            status = 'running', stage = 'facts', updated_at = now() \
          WHERE run_id = $1 AND status = 'queued' AND stage = 'starting' \
          RETURNING run_id, repo_id, status, stage, \
                    commits_emitted, files_emitted, chunks_emitted, chunks_reused, \
                    chunks_tombstoned, ast_edges_emitted, abstractions_emitted, \
                    embeddings_landed, citations_emitted, \
                    error_message, started_at, updated_at, finished_at",
    )
    .bind(run_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(Into::into))
}

/// Mark a run succeeded and return the terminal snapshot.
///
/// # Errors
/// Returns `RunNotFound`, `RunAlreadyTerminal`, or database errors.
pub async fn mark_succeeded(
    pool: &PgPool,
    run_id: Uuid,
    counters: &StageCounters,
) -> Result<RepoIngestionRun, RepoRegistryError> {
    let row = sqlx::query_as::<_, RunRow>(
        "UPDATE proxima_code.repo_ingestion_runs SET \
            status = 'succeeded', stage = 'done', \
            commits_emitted = $2, files_emitted = $3, chunks_emitted = $4, \
            chunks_reused = $5, chunks_tombstoned = $6, ast_edges_emitted = $7, \
            abstractions_emitted = $8, embeddings_landed = $9, citations_emitted = $10, \
            updated_at = now(), finished_at = now() \
          WHERE run_id = $1 AND status NOT IN ('succeeded', 'failed') \
          RETURNING run_id, repo_id, status, stage, \
                    commits_emitted, files_emitted, chunks_emitted, chunks_reused, \
                    chunks_tombstoned, ast_edges_emitted, abstractions_emitted, \
                    embeddings_landed, citations_emitted, \
                    error_message, started_at, updated_at, finished_at",
    )
    .bind(run_id)
    .bind(i32_from_u32(counters.commits_emitted))
    .bind(i32_from_u32(counters.files_emitted))
    .bind(i32_from_u32(counters.chunks_emitted))
    .bind(i32_from_u32(counters.chunks_reused))
    .bind(i32_from_u32(counters.chunks_tombstoned))
    .bind(i32_from_u32(counters.ast_edges_emitted))
    .bind(i32_from_u32(counters.abstractions_emitted))
    .bind(i32_from_u32(counters.embeddings_landed))
    .bind(i32_from_u32(counters.citations_emitted))
    .fetch_optional(pool)
    .await?;

    if let Some(row) = row {
        Ok(row.into())
    } else {
        terminal_or_not_found(pool, run_id).await
    }
}

/// Mark every queued/running run as failed.
///
/// Intended to be called once at process boot under the single-writer
/// invariant: any active run in the DB belongs to a prior process whose
/// in-memory driver and event hub are gone, so the row is unreachable and
/// must be retired before it blocks new runs through the partial unique
/// index `repo_ingestion_runs_one_active`.
///
/// Returns the number of rows transitioned.
///
/// # Errors
/// Returns `RepoRegistryError::Database` on database failures.
pub async fn sweep_orphaned_runs(pool: &PgPool) -> Result<u64, RepoRegistryError> {
    let result = sqlx::query(
        "UPDATE proxima_code.repo_ingestion_runs SET \
            status = 'failed', \
            error_message = 'abandoned by process restart', \
            updated_at = now(), \
            finished_at = now() \
          WHERE status IN ('queued', 'running')",
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Mark a run failed and return the terminal snapshot.
///
/// # Errors
/// Returns `RunNotFound`, `RunAlreadyTerminal`, or database errors.
pub async fn mark_failed(
    pool: &PgPool,
    run_id: Uuid,
    error_message: &str,
) -> Result<RepoIngestionRun, RepoRegistryError> {
    let row = sqlx::query_as::<_, RunRow>(
        "UPDATE proxima_code.repo_ingestion_runs SET \
            status = 'failed', error_message = $2, updated_at = now(), finished_at = now() \
          WHERE run_id = $1 AND status NOT IN ('succeeded', 'failed') \
          RETURNING run_id, repo_id, status, stage, \
                    commits_emitted, files_emitted, chunks_emitted, chunks_reused, \
                    chunks_tombstoned, ast_edges_emitted, abstractions_emitted, \
                    embeddings_landed, citations_emitted, \
                    error_message, started_at, updated_at, finished_at",
    )
    .bind(run_id)
    .bind(error_message)
    .fetch_optional(pool)
    .await?;

    if let Some(row) = row {
        Ok(row.into())
    } else {
        terminal_or_not_found(pool, run_id).await
    }
}

async fn terminal_or_not_found(
    pool: &PgPool,
    run_id: Uuid,
) -> Result<RepoIngestionRun, RepoRegistryError> {
    match get_run(pool, run_id).await? {
        Some(run) if matches!(run.status, RunStatus::Succeeded | RunStatus::Failed) => {
            Err(RepoRegistryError::RunAlreadyTerminal {
                run_id,
                status: run.status,
            })
        }
        Some(run) => Ok(run),
        None => Err(RepoRegistryError::RunNotFound { run_id }),
    }
}

fn i32_from_u32(v: u32) -> i32 {
    i32::try_from(v).unwrap_or(i32::MAX)
}

fn u32_from_i32(v: i32) -> u32 {
    u32::try_from(v).unwrap_or(0)
}

#[derive(Debug, sqlx::FromRow)]
struct RepoRow {
    repo_id: Uuid,
    canonical_path: String,
    display_name: String,
    last_cursor: Option<Vec<u8>>,
    last_polled_at: Option<time::OffsetDateTime>,
    created_at: time::OffsetDateTime,
}

#[derive(Debug, sqlx::FromRow)]
struct RunRow {
    run_id: Uuid,
    repo_id: Uuid,
    status: String,
    stage: String,
    commits_emitted: i32,
    files_emitted: i32,
    chunks_emitted: i32,
    chunks_reused: i32,
    chunks_tombstoned: i32,
    ast_edges_emitted: i32,
    abstractions_emitted: i32,
    embeddings_landed: i32,
    citations_emitted: i32,
    error_message: Option<String>,
    started_at: time::OffsetDateTime,
    updated_at: time::OffsetDateTime,
    finished_at: Option<time::OffsetDateTime>,
}

impl From<RunRow> for RepoIngestionRun {
    fn from(row: RunRow) -> Self {
        let status = RunStatus::from_str(&row.status)
            .unwrap_or_else(|err| panic!("invalid persisted ingestion run status: {err}"));
        let stage = RunStage::from_str(&row.stage)
            .unwrap_or_else(|err| panic!("invalid persisted ingestion run stage: {err}"));
        Self {
            run_id: row.run_id,
            repo_id: row.repo_id,
            status,
            stage,
            commits_emitted: u32_from_i32(row.commits_emitted),
            files_emitted: u32_from_i32(row.files_emitted),
            chunks_emitted: u32_from_i32(row.chunks_emitted),
            chunks_reused: u32_from_i32(row.chunks_reused),
            chunks_tombstoned: u32_from_i32(row.chunks_tombstoned),
            ast_edges_emitted: u32_from_i32(row.ast_edges_emitted),
            abstractions_emitted: u32_from_i32(row.abstractions_emitted),
            embeddings_landed: u32_from_i32(row.embeddings_landed),
            citations_emitted: u32_from_i32(row.citations_emitted),
            error_message: row.error_message,
            started_at: row.started_at,
            updated_at: row.updated_at,
            finished_at: row.finished_at,
        }
    }
}

impl From<RepoRow> for RepoRecord {
    fn from(row: RepoRow) -> Self {
        Self {
            repo_id: row.repo_id,
            canonical_path: row.canonical_path,
            display_name: row.display_name,
            last_cursor: row.last_cursor,
            last_polled_at: row.last_polled_at,
            created_at: row.created_at,
        }
    }
}
