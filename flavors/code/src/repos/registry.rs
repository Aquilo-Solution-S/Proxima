use super::records::{RepoEraseReceipt, RepoRecord, RepoRegistryError};
use super::rows::RepoRow;
use proxima_core::verbs::schema::{PayloadKind, SchemaInfo};
use proxima_core::{EntityKind, Owner, sidecar_tables};
use proxima_storage_pg::verbs::hard_delete::{
    HardDeleteSet, HardDeleteSidecars, execute_hard_delete,
};
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
    let (kind, principal_id, org_id) = owner.columns();

    let rows = sqlx::query_as::<_, RepoRow>(
        "SELECT repo_id, canonical_path, display_name, target_branch, last_cursor, last_polled_at, created_at \
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
    let (kind, principal_id, org_id) = owner.columns();
    let target_branch = detect_target_branch(canonical_path);

    let row = sqlx::query_as::<_, RepoRow>(
        "INSERT INTO proxima_code.repos \
            (owner_principal_kind, owner_principal_id, owner_org_id, \
             repo_id, canonical_path, display_name, target_branch, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, now()) \
         ON CONFLICT (owner_principal_kind, owner_principal_id, owner_org_id, canonical_path) \
         DO NOTHING \
         RETURNING repo_id, canonical_path, display_name, target_branch, last_cursor, last_polled_at, created_at",
    )
    .bind(kind)
    .bind(principal_id)
    .bind(org_id)
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
    let (kind, principal_id, org_id) = owner.columns();

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
/// for `owner`; `RepoRegistryError::Database` on database failures;
/// `RepoRegistryError::Storage` on shared hard-delete failures.
#[allow(clippy::too_many_lines)]
pub async fn erase_repo(
    pool: &PgPool,
    owner: &Owner,
    repo_id: Uuid,
    schemas: &[SchemaInfo],
) -> Result<RepoEraseReceipt, RepoRegistryError> {
    let (kind, principal_id, org_id) = owner.columns();
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
         JOIN proxima_code.work_requested_v1 s USING (memory_id) \
         WHERE m.owner_principal_kind = $1 \
           AND m.owner_principal_id = $2 \
           AND m.owner_org_id = $3 \
           AND s.repo_id = $4 \
         UNION \
         SELECT m.memory_id \
         FROM proxima_core.memories m \
         JOIN proxima_code.test_requested_v1 s USING (memory_id) \
         WHERE m.owner_principal_kind = $1 \
           AND m.owner_principal_id = $2 \
           AND m.owner_org_id = $3 \
           AND s.repo_id = $4 \
         UNION \
         SELECT m.memory_id \
         FROM proxima_core.memories m \
         JOIN proxima_code.acceptance_criteria_v1 s USING (memory_id) \
         JOIN proxima_code.work_requested_v1 r \
           ON r.memory_id = s.work_item_memory_id \
         WHERE m.owner_principal_kind = $1 \
           AND m.owner_principal_id = $2 \
           AND m.owner_org_id = $3 \
           AND r.repo_id = $4 \
         UNION \
         SELECT m.memory_id \
         FROM proxima_core.memories m \
         JOIN proxima_code.execution_result_v1 s USING (memory_id) \
         WHERE m.owner_principal_kind = $1 \
           AND m.owner_principal_id = $2 \
           AND m.owner_org_id = $3 \
           AND s.repo_id = $4 \
         UNION \
         SELECT m.memory_id \
         FROM proxima_core.memories m \
         JOIN proxima_code.test_result_v1 s USING (memory_id) \
         WHERE m.owner_principal_kind = $1 \
           AND m.owner_principal_id = $2 \
           AND m.owner_org_id = $3 \
           AND s.repo_id = $4 \
         UNION \
         SELECT m.memory_id \
         FROM proxima_core.memories m \
         JOIN proxima_code.acceptance_verification_v1 s USING (memory_id) \
         JOIN proxima_core.memories wi ON wi.memory_id = s.work_item_memory_id \
         LEFT JOIN proxima_code.work_requested_v1 wr ON wr.memory_id = wi.memory_id \
         LEFT JOIN proxima_code.test_requested_v1 tr ON tr.memory_id = wi.memory_id \
         WHERE m.owner_principal_kind = $1 \
           AND m.owner_principal_id = $2 \
           AND m.owner_org_id = $3 \
           AND COALESCE(wr.repo_id, tr.repo_id) = $4",
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
         JOIN proxima_code.code_chunk_v1 s USING (memory_id) \
         WHERE m.owner_principal_kind = $1 \
           AND m.owner_principal_id = $2 \
           AND m.owner_org_id = $3 \
           AND s.repo_id = $4 \
         UNION \
         SELECT m.memory_id \
         FROM proxima_core.memories m \
         JOIN proxima_code.commit_summary_v1 s USING (memory_id) \
         WHERE m.owner_principal_kind = $1 \
           AND m.owner_principal_id = $2 \
           AND m.owner_org_id = $3 \
           AND s.repo_id = $4 \
         UNION \
         SELECT m.memory_id \
         FROM proxima_core.memories m \
         JOIN proxima_code.execution_plan_v1 s USING (memory_id) \
         WHERE m.owner_principal_kind = $1 \
           AND m.owner_principal_id = $2 \
           AND m.owner_org_id = $3 \
           AND s.repo_id = $4 \
         UNION \
         SELECT m.memory_id \
         FROM proxima_core.memories m \
         JOIN proxima_code.acceptance_summary_v1 s USING (memory_id) \
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

    let fact_ids = sqlx::query_scalar::<_, Uuid>("SELECT memory_id FROM tmp_proxima_repo_facts")
        .fetch_all(&mut *tx)
        .await?;
    let abstraction_ids =
        sqlx::query_scalar::<_, Uuid>("SELECT memory_id FROM tmp_proxima_repo_abstractions")
            .fetch_all(&mut *tx)
            .await?;
    let edge_ids = sqlx::query_scalar::<_, Uuid>("SELECT edge_id FROM tmp_proxima_repo_edges")
        .fetch_all(&mut *tx)
        .await?;
    let event_ids =
        sqlx::query_scalar::<_, Vec<u8>>("SELECT event_id FROM tmp_proxima_repo_events")
            .fetch_all(&mut *tx)
            .await?;

    let mut memories = Vec::new();
    memories.extend(fact_ids.into_iter().map(|id| (EntityKind::Fact, id)));
    memories.extend(
        abstraction_ids
            .into_iter()
            .map(|id| (EntityKind::Abstraction, id)),
    );
    let set = HardDeleteSet {
        memories,
        edge_ids,
        fact_entity_ids: Vec::new(),
        event_ids,
    };

    let mut memory_keyed = sidecar_tables(schemas, PayloadKind::Fact);
    memory_keyed.extend(sidecar_tables(schemas, PayloadKind::Abstraction));
    memory_keyed.sort();
    memory_keyed.dedup();
    let edge_keyed = sidecar_tables(schemas, PayloadKind::Edge);
    let citation_mapping_keyed = sidecar_tables(schemas, PayloadKind::CitationMapping);
    let sidecars = HardDeleteSidecars {
        memory_keyed: &memory_keyed,
        edge_keyed: &edge_keyed,
        citation_mapping_keyed: &citation_mapping_keyed,
    };

    let counts = execute_hard_delete(&mut tx, &set, &sidecars).await?;
    receipt.edges_deleted = counts.edges;
    receipt.embeddings_deleted = counts.embeddings;
    receipt.citation_mappings_deleted = counts.citation_mappings;
    receipt.events_deleted = counts.events;

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
    let (kind, principal_id, org_id) = owner.columns();

    let row = sqlx::query_as::<_, RepoRow>(
        "SELECT repo_id, canonical_path, display_name, target_branch, last_cursor, last_polled_at, created_at \
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

async fn update_target_branch(
    pool: &PgPool,
    owner: &Owner,
    repo_id: Uuid,
    target_branch: Option<&str>,
) -> Result<RepoRecord, RepoRegistryError> {
    let (kind, principal_id, org_id) = owner.columns();
    let row = sqlx::query_as::<_, RepoRow>(
        "UPDATE proxima_code.repos \
         SET target_branch = $5 \
         WHERE owner_principal_kind = $1 \
           AND owner_principal_id = $2 \
           AND owner_org_id = $3 \
           AND repo_id = $4 \
         RETURNING repo_id, canonical_path, display_name, target_branch, last_cursor, last_polled_at, created_at",
    )
    .bind(kind)
    .bind(principal_id)
    .bind(org_id)
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
    let (kind, principal_id, org_id) = owner.columns();

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
