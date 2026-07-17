#![allow(dead_code)]

use super::records::{RepoEraseReceipt, RepoRecord, RepoRegistryError};
use super::rows::RepoRow;
use proxima_core::Owner;
use proxima_core::verbs::schema::SchemaInfo;
use sqlx::PgPool;
use uuid::Uuid;

/// List all repos registered for `owner`, oldest first.
///
/// # Errors
/// Returns `RepoRegistryError::Database` on database failures.
pub async fn list_repos(
    pool: &PgPool,
    owner: &Owner,
) -> Result<Vec<RepoRecord>, RepoRegistryError> {
    let (kind, principal_id) = owner.columns();

    let rows = sqlx::query_as::<_, RepoRow>(
        "SELECT repo_id, canonical_path, display_name, target_branch, last_cursor, last_polled_at, created_at \
         FROM proxima_code.repos \
         WHERE owner_kind = $1 \
           AND owner_id = $2 \
         ORDER BY created_at ASC",
    )
    .bind(kind)
    .bind(principal_id)
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().map(Into::into).collect())
}

/// One page of `owner`'s repos in the keyset total order
/// `(created_at ASC, repo_id ASC)`, starting strictly after `after` when
/// given. Fetches at most `limit` rows; callers over-fetch by one to
/// detect further pages.
///
/// # Errors
/// Returns `RepoRegistryError::Database` on database failures.
pub async fn list_repos_page(
    pool: &PgPool,
    owner: &Owner,
    after: Option<(time::OffsetDateTime, Uuid)>,
    limit: i64,
) -> Result<Vec<RepoRecord>, RepoRegistryError> {
    let (kind, principal_id) = owner.columns();
    let after_created_at = after.map(|(created_at, _)| created_at);
    let after_repo_id = after.map(|(_, repo_id)| repo_id);

    let rows = sqlx::query_as::<_, RepoRow>(
        "SELECT repo_id, canonical_path, display_name, target_branch, last_cursor, last_polled_at, created_at \
         FROM proxima_code.repos \
         WHERE owner_kind = $1 \
           AND owner_id = $2 \
           AND ($3::timestamptz IS NULL \
                OR (created_at, repo_id) > ($3::timestamptz, $4::uuid)) \
         ORDER BY created_at ASC, repo_id ASC \
         LIMIT $5",
    )
    .bind(kind)
    .bind(principal_id)
    .bind(after_created_at)
    .bind(after_repo_id)
    .bind(limit)
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
    let (kind, principal_id) = owner.columns();
    let target_branch = detect_target_branch(canonical_path);

    let row = sqlx::query_as::<_, RepoRow>(
        "INSERT INTO proxima_code.repos \
            (owner_kind, owner_id, \
             repo_id, canonical_path, display_name, target_branch, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, now()) \
         ON CONFLICT (owner_kind, owner_id, canonical_path) \
         DO NOTHING \
         RETURNING repo_id, canonical_path, display_name, target_branch, last_cursor, last_polled_at, created_at",
    )
    .bind(kind)
    .bind(principal_id)
    .bind(repo_id)
    .bind(canonical_path)
    .bind(display_name)
    .bind(target_branch)
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
             WHERE owner_kind = $1 \
               AND owner_id = $2 \
               AND canonical_path = $3 \
         )",
    )
    .bind(kind)
    .bind(principal_id)
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

/// Persist the target branch used by workspace-mode runs for a repo.
///
/// # Errors
/// Returns `RepoRegistryError::NotFound` if the repo is not registered,
/// `RepoRegistryError::InvalidTargetBranch` if a non-empty branch cannot
/// resolve in the local Git worktree, or `RepoRegistryError::Database` on
/// database failures.
pub async fn set_repo_target_branch(
    pool: &PgPool,
    owner: &Owner,
    repo_id: Uuid,
    target_branch: Option<&str>,
) -> Result<RepoRecord, RepoRegistryError> {
    let target_branch = target_branch.and_then(normalize_target_branch);
    let record = get_repo(pool, owner, repo_id)
        .await?
        .ok_or(RepoRegistryError::NotFound { repo_id })?;
    if let Some(branch) = target_branch {
        verify_target_branch(repo_id, &record.canonical_path, branch)?;
    }
    update_target_branch(pool, owner, repo_id, target_branch).await
}

/// Fill a legacy `NULL` target branch from the worktree's current branch.
///
/// # Errors
/// Returns `RepoRegistryError::NotFound` if the repo is not registered,
/// `RepoRegistryError::InvalidTargetBranch` if the worktree is detached or
/// the inferred branch cannot resolve, or `RepoRegistryError::Database` on
/// database failures.
pub async fn infer_missing_target_branch(
    pool: &PgPool,
    owner: &Owner,
    repo_id: Uuid,
) -> Result<RepoRecord, RepoRegistryError> {
    let record = get_repo(pool, owner, repo_id)
        .await?
        .ok_or(RepoRegistryError::NotFound { repo_id })?;
    if record.target_branch.is_some() {
        return Ok(record);
    }
    let target_branch = detect_target_branch(&record.canonical_path).ok_or_else(|| {
        RepoRegistryError::InvalidTargetBranch {
            repo_id,
            target_branch: "<current HEAD>".to_string(),
            reason: "worktree has no symbolic branch".to_string(),
        }
    })?;
    verify_target_branch(repo_id, &record.canonical_path, &target_branch)?;
    update_target_branch(pool, owner, repo_id, Some(&target_branch)).await
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
    let (kind, principal_id) = owner.columns();

    let result = sqlx::query(
        "DELETE FROM proxima_code.repos \
         WHERE owner_kind = $1 \
           AND owner_id = $2 \
           AND repo_id = $3",
    )
    .bind(kind)
    .bind(principal_id)
    .bind(repo_id)
    .execute(pool)
    .await?;

    Ok(result.rows_affected() > 0)
}

/// Erase one registered repo's code-flavor rows and owned substrate rows.
///
/// # Errors
/// Returns `RepoRegistryError::NotFound` if the repo is not registered
/// for `owner`; otherwise returns `RepoRegistryError::Database` on
/// database failures.
pub async fn erase_repo(
    pool: &PgPool,
    owner: &Owner,
    repo_id: Uuid,
    _schemas: &[SchemaInfo],
) -> Result<RepoEraseReceipt, RepoRegistryError> {
    let outcome = proxima_storage_pg::verbs::code_repo_erase::erase_code_repo(pool, owner, repo_id)
        .await?
        .ok_or(RepoRegistryError::NotFound { repo_id })?;
    Ok(RepoEraseReceipt {
        repo_id: outcome.repo_id,
        completed_at: outcome.completed_at,
        facts_deleted: outcome.facts_deleted,
        abstractions_deleted: outcome.abstractions_deleted,
        edges_deleted: outcome.edges_deleted,
        embeddings_deleted: outcome.embeddings_deleted,
        receipts_deleted: outcome.receipts_deleted,
        citation_mappings_deleted: outcome.citation_mappings_deleted,
        cited_objects_deleted: outcome.cited_objects_deleted,
        source_batches_deleted: outcome.source_batches_deleted,
        repo_record_deleted: outcome.repo_record_deleted,
    })
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
    let (kind, principal_id) = owner.columns();

    let row = sqlx::query_as::<_, RepoRow>(
        "SELECT repo_id, canonical_path, display_name, target_branch, last_cursor, last_polled_at, created_at \
         FROM proxima_code.repos \
         WHERE owner_kind = $1 \
           AND owner_id = $2 \
           AND repo_id = $3",
    )
    .bind(kind)
    .bind(principal_id)
    .bind(repo_id)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(Into::into))
}

async fn update_target_branch(
    pool: &PgPool,
    owner: &Owner,
    repo_id: Uuid,
    target_branch: Option<&str>,
) -> Result<RepoRecord, RepoRegistryError> {
    let (kind, principal_id) = owner.columns();
    let row = sqlx::query_as::<_, RepoRow>(
        "UPDATE proxima_code.repos \
         SET target_branch = $4 \
         WHERE owner_kind = $1 \
           AND owner_id = $2 \
           AND repo_id = $3 \
         RETURNING repo_id, canonical_path, display_name, target_branch, last_cursor, last_polled_at, created_at",
    )
    .bind(kind)
    .bind(principal_id)
    .bind(repo_id)
    .bind(target_branch)
    .fetch_optional(pool)
    .await?;

    row.map(Into::into)
        .ok_or(RepoRegistryError::NotFound { repo_id })
}

fn detect_target_branch(canonical_path: &str) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(canonical_path)
        .arg("symbolic-ref")
        .arg("--short")
        .arg("HEAD")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!branch.is_empty()).then_some(branch)
}

fn normalize_target_branch(branch: &str) -> Option<&str> {
    let branch = branch.trim();
    (!branch.is_empty()).then_some(branch)
}

fn verify_target_branch(
    repo_id: Uuid,
    canonical_path: &str,
    target_branch: &str,
) -> Result<(), RepoRegistryError> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(canonical_path)
        .arg("rev-parse")
        .arg("--verify")
        .arg(format!("refs/heads/{target_branch}^{{commit}}"))
        .output()
        .map_err(|err| RepoRegistryError::InvalidTargetBranch {
            repo_id,
            target_branch: target_branch.to_string(),
            reason: err.to_string(),
        })?;
    if output.status.success() {
        return Ok(());
    }
    Err(RepoRegistryError::InvalidTargetBranch {
        repo_id,
        target_branch: target_branch.to_string(),
        reason: String::from_utf8_lossy(&output.stderr).trim().to_string(),
    })
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
    let (kind, principal_id) = owner.columns();

    sqlx::query(
        "UPDATE proxima_code.repos \
         SET last_cursor = $3, last_polled_at = $4 \
         WHERE owner_kind = $1 \
           AND owner_id = $2 \
           AND repo_id = $5",
    )
    .bind(kind)
    .bind(principal_id)
    .bind(cursor_bytes)
    .bind(polled_at)
    .bind(repo_id)
    .execute(pool)
    .await?;

    Ok(())
}
