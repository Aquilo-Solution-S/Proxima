//! Repo registry — tracks local-path git repos registered by the user
//! for ingestion. One row per (`Owner`, `canonical_path`). The cursor
//! advances on each successful `run_poll`, persisted via `update_cursor`.

use proxima_core::{Owner, Principal};
use sqlx::PgPool;
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

#[derive(Debug, thiserror::Error)]
pub enum RepoRegistryError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("duplicate repo at path: {canonical_path}")]
    DuplicatePath { canonical_path: String },
    #[error("repo not found: {repo_id}")]
    NotFound { repo_id: Uuid },
}

/// Encode `Owner` into the three column values used by the `repos` table.
fn owner_columns(owner: &Owner) -> (&'static str, uuid::Uuid, uuid::Uuid) {
    let (kind, principal_id) = match &owner.principal {
        Principal::User(u) => ("User", u.into_inner()),
        Principal::Group(g) => ("Group", g.into_inner()),
    };
    (kind, principal_id, owner.org_id.into_inner())
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

#[derive(Debug, sqlx::FromRow)]
struct RepoRow {
    repo_id: Uuid,
    canonical_path: String,
    display_name: String,
    last_cursor: Option<Vec<u8>>,
    last_polled_at: Option<time::OffsetDateTime>,
    created_at: time::OffsetDateTime,
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
