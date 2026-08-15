use proxima_core::{Owner, OwnerRefKind, StorageError};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::error::map_err;

type Tx<'a> = Transaction<'a, Postgres>;

#[derive(Debug, Clone)]
pub struct CodeRepoEraseOutcome {
    pub repo_id: Uuid,
    pub completed_at: time::OffsetDateTime,
    pub facts_deleted: u64,
    pub abstractions_deleted: u64,
    pub edges_deleted: u64,
    pub embeddings_deleted: u64,
    pub receipts_deleted: u64,
    pub citation_mappings_deleted: u64,
    pub cited_objects_deleted: u64,
    pub source_batches_deleted: u64,
    pub repo_record_deleted: bool,
}

/// Erase one code-flavor repo and its selected owner-scoped substrate rows.
///
/// Returns `Ok(None)` when the repo record does not exist for `owner`.
///
/// # Errors
///
/// Returns `StorageError::Internal` / constraint-mapped storage errors for
/// database failures while selecting or deleting repo-owned rows.
pub async fn erase_code_repo(
    pool: &PgPool,
    owner: &Owner,
    repo_id: Uuid,
) -> Result<Option<CodeRepoEraseOutcome>, StorageError> {
    erase_code_repo_inner(pool, owner, repo_id)
        .await
        .map_err(map_err)
}

async fn erase_code_repo_inner(
    pool: &PgPool,
    owner: &Owner,
    repo_id: Uuid,
) -> Result<Option<CodeRepoEraseOutcome>, sqlx::Error> {
    let (kind, principal_id) = owner.columns();
    let mut tx = pool.begin().await?;
    sqlx::query("SET CONSTRAINTS ALL DEFERRED")
        .execute(&mut *tx)
        .await?;

    if !repo_exists(&mut tx, kind, principal_id, repo_id).await? {
        return Ok(None);
    }

    create_repo_erase_sets(&mut tx, kind, principal_id, repo_id).await?;
    repoint_surviving_fact_entity_heads(&mut tx).await?;
    create_selected_fact_entities(&mut tx).await?;
    create_selected_receipts(&mut tx).await?;
    create_selected_source_batches(&mut tx).await?;
    create_selected_citations(&mut tx).await?;
    create_selected_edges(&mut tx).await?;

    let facts_deleted = selected_fact_memory_count(&mut tx).await?;
    let abstractions_deleted = selected_derived_memory_count(&mut tx).await?;
    delete_flavor_sidecars(&mut tx).await?;

    let embeddings_deleted = execute_query(&mut tx, sqlx::query(DELETE_EMBEDDING_JOBS_SQL))
        .await?
        .saturating_add(execute_query(&mut tx, sqlx::query(DELETE_EMBEDDING_HEADS_SQL)).await?)
        .saturating_add(execute_query(&mut tx, sqlx::query(DELETE_EMBEDDINGS_SQL)).await?);

    delete_core_memory_sidecars(&mut tx).await?;
    let edges_deleted = execute_query(&mut tx, sqlx::query(DELETE_SELECTED_EDGES_SQL)).await?;
    execute_query(&mut tx, sqlx::query(DELETE_CHANGE_EVENTS_SQL)).await?;
    delete_citation_sidecars(&mut tx).await?;
    let citation_mappings_deleted =
        execute_query(&mut tx, sqlx::query(DELETE_CITATION_MAPPINGS_SQL)).await?;
    let cited_objects_deleted =
        execute_query(&mut tx, sqlx::query(DELETE_CITED_OBJECTS_SQL)).await?;
    execute_query(&mut tx, sqlx::query(DELETE_FACT_ENTITIES_SQL)).await?;
    execute_query(&mut tx, sqlx::query(DELETE_SELECTED_MEMORIES_SQL)).await?;
    let receipts_deleted = execute_query(&mut tx, sqlx::query(DELETE_FACT_RECEIPTS_SQL)).await?;
    let source_batches_deleted =
        execute_query(&mut tx, sqlx::query(DELETE_SOURCE_BATCHES_SQL)).await?;
    let repo_record_deleted = delete_repo_record(&mut tx, kind, principal_id, repo_id).await? > 0;
    let completed_at = time::OffsetDateTime::now_utc();
    tx.commit().await?;

    Ok(Some(CodeRepoEraseOutcome {
        repo_id,
        completed_at,
        facts_deleted,
        abstractions_deleted,
        edges_deleted,
        embeddings_deleted,
        receipts_deleted,
        citation_mappings_deleted,
        cited_objects_deleted,
        source_batches_deleted,
        repo_record_deleted,
    }))
}

async fn repo_exists(
    tx: &mut Tx<'_>,
    owner_kind: OwnerRefKind,
    owner_id: Option<Uuid>,
    repo_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let exists: Option<(Uuid,)> = sqlx::query_as(
        "SELECT repo_id
           FROM proxima_code.repos
          WHERE owner_kind = $1
            AND owner_id IS NOT DISTINCT FROM $2
            AND repo_id = $3",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .bind(repo_id)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(exists.is_some())
}

async fn create_repo_erase_sets(
    tx: &mut Tx<'_>,
    owner_kind: OwnerRefKind,
    owner_id: Option<Uuid>,
    repo_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query("CREATE TEMP TABLE erase_repo_memories(memory_id uuid PRIMARY KEY) ON COMMIT DROP")
        .execute(&mut **tx)
        .await?;
    execute_owner_repo_query(
        tx,
        sqlx::query(SELECT_REPO_CODE_CHUNK_SQL),
        owner_kind,
        owner_id,
        repo_id,
    )
    .await?;
    execute_owner_repo_query(
        tx,
        sqlx::query(SELECT_REPO_COMMIT_SUMMARY_SQL),
        owner_kind,
        owner_id,
        repo_id,
    )
    .await?;
    execute_owner_repo_query(
        tx,
        sqlx::query(SELECT_REPO_COMMIT_SQL),
        owner_kind,
        owner_id,
        repo_id,
    )
    .await?;
    execute_owner_repo_query(
        tx,
        sqlx::query(SELECT_REPO_DEVELOPMENT_PERSPECTIVE_SQL),
        owner_kind,
        owner_id,
        repo_id,
    )
    .await?;
    execute_owner_repo_query(
        tx,
        sqlx::query(SELECT_REPO_WORK_REQUESTED_SQL),
        owner_kind,
        owner_id,
        repo_id,
    )
    .await?;
    execute_owner_repo_query(
        tx,
        sqlx::query(SELECT_REPO_FILE_REVISION_SQL),
        owner_kind,
        owner_id,
        repo_id,
    )
    .await?;
    execute_owner_repo_query(
        tx,
        sqlx::query(SELECT_REPO_TEST_REQUESTED_SQL),
        owner_kind,
        owner_id,
        repo_id,
    )
    .await?;
    execute_owner_repo_query(
        tx,
        sqlx::query(SELECT_REPO_EXECUTION_PLAN_SQL),
        owner_kind,
        owner_id,
        repo_id,
    )
    .await?;
    execute_owner_repo_query(
        tx,
        sqlx::query(SELECT_REPO_EXECUTION_RESULT_SQL),
        owner_kind,
        owner_id,
        repo_id,
    )
    .await?;
    execute_owner_repo_query(
        tx,
        sqlx::query(SELECT_REPO_TEST_RESULT_SQL),
        owner_kind,
        owner_id,
        repo_id,
    )
    .await?;
    execute_owner_repo_query(
        tx,
        sqlx::query(SELECT_REPO_ACCEPTANCE_SUMMARY_SQL),
        owner_kind,
        owner_id,
        repo_id,
    )
    .await?;
    execute_query(tx, sqlx::query(SELECT_REPO_ACCEPTANCE_CRITERIA_CHILD_SQL)).await?;
    execute_query(
        tx,
        sqlx::query(SELECT_REPO_ACCEPTANCE_VERIFICATION_CHILD_SQL),
    )
    .await?;
    Ok(())
}

async fn repoint_surviving_fact_entity_heads(tx: &mut Tx<'_>) -> Result<(), sqlx::Error> {
    sqlx::query(
        "WITH surviving_heads AS (
             SELECT DISTINCT ON (fe.fact_entity_id)
                    fe.fact_entity_id, survivor.memory_id, survivor.created_at
               FROM proxima_core.fact_entities fe
               JOIN erase_repo_memories selected_current
                 ON selected_current.memory_id = fe.current_memory_id
               JOIN proxima_core.memories survivor
                 ON survivor.fact_entity_id = fe.fact_entity_id
              WHERE NOT EXISTS (
                    SELECT 1
                      FROM erase_repo_memories selected
                     WHERE selected.memory_id = survivor.memory_id
                )
              ORDER BY fe.fact_entity_id, survivor.created_at DESC, survivor.memory_id DESC
         )
         UPDATE proxima_core.fact_entities fe
            SET current_memory_id = surviving_heads.memory_id,
                current_created_at = surviving_heads.created_at
           FROM surviving_heads
          WHERE fe.fact_entity_id = surviving_heads.fact_entity_id",
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn create_selected_fact_entities(tx: &mut Tx<'_>) -> Result<(), sqlx::Error> {
    sqlx::query(
        "CREATE TEMP TABLE erase_repo_fact_entities(fact_entity_id uuid PRIMARY KEY) ON COMMIT DROP",
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "INSERT INTO erase_repo_fact_entities(fact_entity_id)
         SELECT DISTINCT m.fact_entity_id
           FROM proxima_core.memories m
           JOIN erase_repo_memories selected ON selected.memory_id = m.memory_id
          WHERE m.fact_entity_id IS NOT NULL
            AND NOT EXISTS (
                SELECT 1
                  FROM proxima_core.memories survivor
                 WHERE survivor.fact_entity_id = m.fact_entity_id
                   AND NOT EXISTS (
                       SELECT 1 FROM erase_repo_memories s
                        WHERE s.memory_id = survivor.memory_id
                   )
            )
         ON CONFLICT DO NOTHING",
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "INSERT INTO erase_repo_fact_entities(fact_entity_id)
         SELECT fe.fact_entity_id
           FROM proxima_core.fact_entities fe
           JOIN erase_repo_memories selected ON selected.memory_id = fe.current_memory_id
          WHERE NOT EXISTS (
                SELECT 1
                  FROM proxima_core.memories survivor
                 WHERE survivor.fact_entity_id = fe.fact_entity_id
                   AND NOT EXISTS (
                       SELECT 1 FROM erase_repo_memories s
                        WHERE s.memory_id = survivor.memory_id
                   )
            )
         ON CONFLICT DO NOTHING",
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn create_selected_receipts(tx: &mut Tx<'_>) -> Result<(), sqlx::Error> {
    sqlx::query(
        "CREATE TEMP TABLE erase_repo_receipts(receipt_id bytea PRIMARY KEY, source_batch_id uuid) ON COMMIT DROP",
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "INSERT INTO erase_repo_receipts(receipt_id, source_batch_id)
         SELECT DISTINCT fr.receipt_id, fr.source_batch_id
           FROM proxima_core.memories m
           JOIN erase_repo_memories selected ON selected.memory_id = m.memory_id
           JOIN proxima_core.fact_receipts fr ON fr.receipt_id = m.receipt_id
          WHERE m.receipt_id IS NOT NULL
         ON CONFLICT DO NOTHING",
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn create_selected_source_batches(tx: &mut Tx<'_>) -> Result<(), sqlx::Error> {
    sqlx::query("CREATE TEMP TABLE erase_repo_source_batches(id uuid PRIMARY KEY) ON COMMIT DROP")
        .execute(&mut **tx)
        .await?;
    sqlx::query(
        "INSERT INTO erase_repo_source_batches(id)
         SELECT DISTINCT candidate.id
           FROM (
                 SELECT source_batch_id AS id FROM erase_repo_receipts WHERE source_batch_id IS NOT NULL
                 UNION
                 SELECT m.source_batch_id AS id
                   FROM proxima_core.memories m
                   JOIN erase_repo_memories selected ON selected.memory_id = m.memory_id
                  WHERE m.source_batch_id IS NOT NULL
           ) candidate
          WHERE NOT EXISTS (
                SELECT 1
                  FROM proxima_core.memories survivor
                 WHERE survivor.source_batch_id = candidate.id
                   AND NOT EXISTS (
                       SELECT 1 FROM erase_repo_memories s
                        WHERE s.memory_id = survivor.memory_id
                   )
            )
            AND NOT EXISTS (
                SELECT 1
                  FROM proxima_core.fact_receipts survivor
                 WHERE survivor.source_batch_id = candidate.id
                   AND NOT EXISTS (
                       SELECT 1 FROM erase_repo_receipts s
                        WHERE s.receipt_id = survivor.receipt_id
                   )
            )
         ON CONFLICT DO NOTHING",
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn create_selected_citations(tx: &mut Tx<'_>) -> Result<(), sqlx::Error> {
    sqlx::query(
        "CREATE TEMP TABLE erase_repo_citation_mappings(citation_mapping_id uuid PRIMARY KEY, cited_object_id uuid NOT NULL) ON COMMIT DROP",
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "INSERT INTO erase_repo_citation_mappings(citation_mapping_id, cited_object_id)
         SELECT DISTINCT cm.citation_mapping_id, cm.cited_object_id
           FROM proxima_core.citation_mappings cm
           JOIN erase_repo_memories selected ON selected.memory_id = cm.memory_id
         ON CONFLICT DO NOTHING",
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "CREATE TEMP TABLE erase_repo_cited_objects(cited_object_id uuid PRIMARY KEY) ON COMMIT DROP",
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "INSERT INTO erase_repo_cited_objects(cited_object_id)
         SELECT DISTINCT cm.cited_object_id
           FROM erase_repo_citation_mappings cm
          WHERE NOT EXISTS (
                SELECT 1
                  FROM proxima_core.citation_mappings survivor
                 WHERE survivor.cited_object_id = cm.cited_object_id
                   AND NOT EXISTS (
                       SELECT 1 FROM erase_repo_citation_mappings selected
                        WHERE selected.citation_mapping_id = survivor.citation_mapping_id
                   )
            )
         ON CONFLICT DO NOTHING",
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Select the index rows an erase takes with it.
///
/// The selection is by endpoint rather than by id, because an edge has no id:
/// the row IS its identity, so the temp table holds the whole primary key.
/// The endpoint columns are also uniform now — one (kind, id) pair per side,
/// with a Fact-entity head addressed the same way as a memory — so the four
/// old columns collapse into two comparisons.
async fn create_selected_edges(tx: &mut Tx<'_>) -> Result<(), sqlx::Error> {
    sqlx::query(
        "CREATE TEMP TABLE erase_repo_edges(
             source_kind proxima_core.edge_endpoint_kind NOT NULL,
             source_id uuid NOT NULL,
             target_kind proxima_core.edge_endpoint_kind NOT NULL,
             target_id uuid NOT NULL,
             kind proxima_core.edge_kind NOT NULL,
             PRIMARY KEY (source_kind, source_id, target_kind, target_id, kind)
         ) ON COMMIT DROP",
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "INSERT INTO erase_repo_edges(source_kind, source_id, target_kind, target_id, kind)
         SELECT e.source_kind, e.source_id, e.target_kind, e.target_id, e.kind
           FROM proxima_core.edges e
          WHERE EXISTS (SELECT 1 FROM erase_repo_memories s WHERE s.memory_id = e.source_id)
             OR EXISTS (SELECT 1 FROM erase_repo_memories s WHERE s.memory_id = e.target_id)
             OR EXISTS (
                 SELECT 1 FROM erase_repo_fact_entities fe
                  WHERE (e.source_kind = 'FactEntityHead'::proxima_core.edge_endpoint_kind
                         AND fe.fact_entity_id = e.source_id)
                     OR (e.target_kind = 'FactEntityHead'::proxima_core.edge_endpoint_kind
                         AND fe.fact_entity_id = e.target_id)
             )
         ON CONFLICT DO NOTHING",
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn delete_flavor_sidecars(tx: &mut Tx<'_>) -> Result<(), sqlx::Error> {
    execute_query(tx, sqlx::query(DELETE_ACCEPTANCE_CRITERION_SIDECAR_SQL)).await?;
    execute_query(tx, sqlx::query(DELETE_TEST_REQUESTED_CRITERION_SIDECAR_SQL)).await?;
    execute_query(tx, sqlx::query(DELETE_EXECUTION_PLAN_ITEM_SIDECAR_SQL)).await?;
    execute_query(tx, sqlx::query(DELETE_ACCEPTANCE_SUMMARY_SIDECAR_SQL)).await?;
    execute_query(tx, sqlx::query(DELETE_ACCEPTANCE_VERIFICATION_SIDECAR_SQL)).await?;
    execute_query(tx, sqlx::query(DELETE_TEST_RESULT_SIDECAR_SQL)).await?;
    execute_query(tx, sqlx::query(DELETE_EXECUTION_RESULT_SIDECAR_SQL)).await?;
    execute_query(tx, sqlx::query(DELETE_EXECUTION_PLAN_SIDECAR_SQL)).await?;
    execute_query(tx, sqlx::query(DELETE_TEST_REQUESTED_SIDECAR_SQL)).await?;
    execute_query(tx, sqlx::query(DELETE_ACCEPTANCE_CRITERIA_SIDECAR_SQL)).await?;
    execute_query(tx, sqlx::query(DELETE_FILE_REVISION_SIDECAR_SQL)).await?;
    execute_query(tx, sqlx::query(DELETE_WORK_REQUESTED_SIDECAR_SQL)).await?;
    execute_query(tx, sqlx::query(DELETE_DEVELOPMENT_PERSPECTIVE_SIDECAR_SQL)).await?;
    execute_query(tx, sqlx::query(DELETE_COMMIT_SIDECAR_SQL)).await?;
    execute_query(tx, sqlx::query(DELETE_COMMIT_SUMMARY_SIDECAR_SQL)).await?;
    execute_query(tx, sqlx::query(DELETE_CODE_CHUNK_CALL_SIDECAR_SQL)).await?;
    execute_query(tx, sqlx::query(DELETE_CODE_CHUNK_SIDECAR_SQL)).await?;
    execute_query(tx, sqlx::query(DELETE_WORK_ASSIGNMENT_SIDECAR_SQL)).await?;
    Ok(())
}

async fn delete_core_memory_sidecars(tx: &mut Tx<'_>) -> Result<(), sqlx::Error> {
    execute_query(tx, sqlx::query(DELETE_MCP_CALL_SIDECAR_SQL)).await?;
    execute_query(tx, sqlx::query(DELETE_AGENT_DERIVATION_SIDECAR_SQL)).await?;
    execute_query(tx, sqlx::query(DELETE_AGENT_NOTE_SIDECAR_SQL)).await?;
    Ok(())
}

async fn delete_citation_sidecars(tx: &mut Tx<'_>) -> Result<(), sqlx::Error> {
    execute_query(tx, sqlx::query(DELETE_CITED_UPLOADS_SQL)).await?;
    execute_query(tx, sqlx::query(DELETE_CITED_UPLOADED_BLOB_SQL)).await?;
    execute_query(tx, sqlx::query(DELETE_CITED_MCP_IO_SQL)).await?;
    Ok(())
}

async fn delete_repo_record(
    tx: &mut Tx<'_>,
    owner_kind: OwnerRefKind,
    owner_id: Option<Uuid>,
    repo_id: Uuid,
) -> Result<u64, sqlx::Error> {
    execute_owner_repo_query(
        tx,
        sqlx::query(
            "DELETE FROM proxima_code.repos
          WHERE owner_kind = $1
            AND owner_id IS NOT DISTINCT FROM $2
            AND repo_id = $3",
        ),
        owner_kind,
        owner_id,
        repo_id,
    )
    .await
}

async fn selected_fact_memory_count(tx: &mut Tx<'_>) -> Result<u64, sqlx::Error> {
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*)::bigint
           FROM erase_repo_memories selected
           JOIN proxima_core.memories m ON m.memory_id = selected.memory_id
          WHERE m.kind = 'Fact'",
    )
    .fetch_one(&mut **tx)
    .await?;
    Ok(count.try_into().unwrap_or_default())
}

async fn selected_derived_memory_count(tx: &mut Tx<'_>) -> Result<u64, sqlx::Error> {
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*)::bigint
           FROM erase_repo_memories selected
           JOIN proxima_core.memories m ON m.memory_id = selected.memory_id
          WHERE m.kind <> 'Fact'",
    )
    .fetch_one(&mut **tx)
    .await?;
    Ok(count.try_into().unwrap_or_default())
}

async fn execute_query(
    tx: &mut Tx<'_>,
    query: sqlx::query::Query<'_, Postgres, sqlx::postgres::PgArguments>,
) -> Result<u64, sqlx::Error> {
    let result = query.execute(&mut **tx).await?;
    Ok(result.rows_affected())
}

async fn execute_owner_repo_query(
    tx: &mut Tx<'_>,
    query: sqlx::query::Query<'_, Postgres, sqlx::postgres::PgArguments>,
    owner_kind: OwnerRefKind,
    owner_id: Option<Uuid>,
    repo_id: Uuid,
) -> Result<u64, sqlx::Error> {
    let result = query
        .bind(owner_kind)
        .bind(owner_id)
        .bind(repo_id)
        .execute(&mut **tx)
        .await?;
    Ok(result.rows_affected())
}

const SELECT_REPO_CODE_CHUNK_SQL: &str = "INSERT INTO erase_repo_memories(memory_id)
     SELECT s.memory_id
       FROM proxima_code.code_chunk_v1 s
       JOIN proxima_core.memories m ON m.memory_id = s.memory_id
      WHERE m.owner_kind = $1 AND m.owner_id IS NOT DISTINCT FROM $2 AND s.repo_id = $3
     ON CONFLICT DO NOTHING";
const SELECT_REPO_COMMIT_SUMMARY_SQL: &str = "INSERT INTO erase_repo_memories(memory_id)
     SELECT s.memory_id
       FROM proxima_code.commit_summary_v1 s
       JOIN proxima_core.memories m ON m.memory_id = s.memory_id
      WHERE m.owner_kind = $1 AND m.owner_id IS NOT DISTINCT FROM $2 AND s.repo_id = $3
     ON CONFLICT DO NOTHING";
const SELECT_REPO_COMMIT_SQL: &str = "INSERT INTO erase_repo_memories(memory_id)
     SELECT s.memory_id
       FROM proxima_code.commit_v1 s
       JOIN proxima_core.memories m ON m.memory_id = s.memory_id
      WHERE m.owner_kind = $1 AND m.owner_id IS NOT DISTINCT FROM $2 AND s.repo_id = $3
     ON CONFLICT DO NOTHING";
const SELECT_REPO_DEVELOPMENT_PERSPECTIVE_SQL: &str = "INSERT INTO erase_repo_memories(memory_id)
     SELECT s.memory_id
       FROM proxima_code.development_perspective_v1 s
       JOIN proxima_core.memories m ON m.memory_id = s.memory_id
      WHERE m.owner_kind = $1 AND m.owner_id IS NOT DISTINCT FROM $2 AND s.repo_id = $3
     ON CONFLICT DO NOTHING";
const SELECT_REPO_WORK_REQUESTED_SQL: &str = "INSERT INTO erase_repo_memories(memory_id)
     SELECT s.memory_id
       FROM proxima_code.work_requested_v1 s
       JOIN proxima_core.memories m ON m.memory_id = s.memory_id
      WHERE m.owner_kind = $1 AND m.owner_id IS NOT DISTINCT FROM $2 AND s.repo_id = $3
     ON CONFLICT DO NOTHING";
const SELECT_REPO_FILE_REVISION_SQL: &str = "INSERT INTO erase_repo_memories(memory_id)
     SELECT s.memory_id
       FROM proxima_code.file_revision_v1 s
       JOIN proxima_core.memories m ON m.memory_id = s.memory_id
      WHERE m.owner_kind = $1 AND m.owner_id IS NOT DISTINCT FROM $2 AND s.repo_id = $3
     ON CONFLICT DO NOTHING";
const SELECT_REPO_TEST_REQUESTED_SQL: &str = "INSERT INTO erase_repo_memories(memory_id)
     SELECT s.memory_id
       FROM proxima_code.test_requested_v1 s
       JOIN proxima_core.memories m ON m.memory_id = s.memory_id
      WHERE m.owner_kind = $1 AND m.owner_id IS NOT DISTINCT FROM $2 AND s.repo_id = $3
     ON CONFLICT DO NOTHING";
const SELECT_REPO_EXECUTION_PLAN_SQL: &str = "INSERT INTO erase_repo_memories(memory_id)
     SELECT s.memory_id
       FROM proxima_code.execution_plan_v1 s
       JOIN proxima_core.memories m ON m.memory_id = s.memory_id
      WHERE m.owner_kind = $1 AND m.owner_id IS NOT DISTINCT FROM $2 AND s.repo_id = $3
     ON CONFLICT DO NOTHING";
const SELECT_REPO_EXECUTION_RESULT_SQL: &str = "INSERT INTO erase_repo_memories(memory_id)
     SELECT s.memory_id
       FROM proxima_code.execution_result_v1 s
       JOIN proxima_core.memories m ON m.memory_id = s.memory_id
      WHERE m.owner_kind = $1 AND m.owner_id IS NOT DISTINCT FROM $2 AND s.repo_id = $3
     ON CONFLICT DO NOTHING";
const SELECT_REPO_TEST_RESULT_SQL: &str = "INSERT INTO erase_repo_memories(memory_id)
     SELECT s.memory_id
       FROM proxima_code.test_result_v1 s
       JOIN proxima_core.memories m ON m.memory_id = s.memory_id
      WHERE m.owner_kind = $1 AND m.owner_id IS NOT DISTINCT FROM $2 AND s.repo_id = $3
     ON CONFLICT DO NOTHING";
const SELECT_REPO_ACCEPTANCE_SUMMARY_SQL: &str = "INSERT INTO erase_repo_memories(memory_id)
     SELECT s.memory_id
       FROM proxima_code.acceptance_summary_v1 s
       JOIN proxima_core.memories m ON m.memory_id = s.memory_id
      WHERE m.owner_kind = $1 AND m.owner_id IS NOT DISTINCT FROM $2 AND s.repo_id = $3
     ON CONFLICT DO NOTHING";

const SELECT_REPO_ACCEPTANCE_CRITERIA_CHILD_SQL: &str = "INSERT INTO erase_repo_memories(memory_id)
     SELECT s.memory_id
       FROM proxima_code.acceptance_criteria_v1 s
      WHERE EXISTS (SELECT 1 FROM erase_repo_memories parent WHERE parent.memory_id = s.work_item_memory_id)
     ON CONFLICT DO NOTHING";
const SELECT_REPO_ACCEPTANCE_VERIFICATION_CHILD_SQL: &str =
    "INSERT INTO erase_repo_memories(memory_id)
     SELECT s.memory_id
       FROM proxima_code.acceptance_verification_v1 s
      WHERE EXISTS (SELECT 1 FROM erase_repo_memories parent WHERE parent.memory_id = s.work_item_memory_id)
         OR EXISTS (SELECT 1 FROM erase_repo_memories verifier WHERE verifier.memory_id = s.verifier_memory_id)
     ON CONFLICT DO NOTHING";

const DELETE_ACCEPTANCE_CRITERION_SIDECAR_SQL: &str =
    "DELETE FROM proxima_code.acceptance_criterion_v1
      WHERE criteria_memory_id IN (SELECT memory_id FROM erase_repo_memories)";
const DELETE_TEST_REQUESTED_CRITERION_SIDECAR_SQL: &str =
    "DELETE FROM proxima_code.test_requested_criterion_v1
      WHERE test_requested_memory_id IN (SELECT memory_id FROM erase_repo_memories)";
const DELETE_EXECUTION_PLAN_ITEM_SIDECAR_SQL: &str =
    "DELETE FROM proxima_code.execution_plan_item_v1
      WHERE plan_memory_id IN (SELECT memory_id FROM erase_repo_memories)";
const DELETE_ACCEPTANCE_SUMMARY_SIDECAR_SQL: &str = "DELETE FROM proxima_code.acceptance_summary_v1
      WHERE memory_id IN (SELECT memory_id FROM erase_repo_memories)";
const DELETE_ACCEPTANCE_VERIFICATION_SIDECAR_SQL: &str =
    "DELETE FROM proxima_code.acceptance_verification_v1
      WHERE memory_id IN (SELECT memory_id FROM erase_repo_memories)";
const DELETE_TEST_RESULT_SIDECAR_SQL: &str = "DELETE FROM proxima_code.test_result_v1
      WHERE memory_id IN (SELECT memory_id FROM erase_repo_memories)";
const DELETE_EXECUTION_RESULT_SIDECAR_SQL: &str = "DELETE FROM proxima_code.execution_result_v1
      WHERE memory_id IN (SELECT memory_id FROM erase_repo_memories)";
const DELETE_EXECUTION_PLAN_SIDECAR_SQL: &str = "DELETE FROM proxima_code.execution_plan_v1
      WHERE memory_id IN (SELECT memory_id FROM erase_repo_memories)";
const DELETE_TEST_REQUESTED_SIDECAR_SQL: &str = "DELETE FROM proxima_code.test_requested_v1
      WHERE memory_id IN (SELECT memory_id FROM erase_repo_memories)";
const DELETE_ACCEPTANCE_CRITERIA_SIDECAR_SQL: &str =
    "DELETE FROM proxima_code.acceptance_criteria_v1
      WHERE memory_id IN (SELECT memory_id FROM erase_repo_memories)";
const DELETE_FILE_REVISION_SIDECAR_SQL: &str = "DELETE FROM proxima_code.file_revision_v1
      WHERE memory_id IN (SELECT memory_id FROM erase_repo_memories)";
const DELETE_WORK_REQUESTED_SIDECAR_SQL: &str = "DELETE FROM proxima_code.work_requested_v1
      WHERE memory_id IN (SELECT memory_id FROM erase_repo_memories)";
const DELETE_DEVELOPMENT_PERSPECTIVE_SIDECAR_SQL: &str =
    "DELETE FROM proxima_code.development_perspective_v1
      WHERE memory_id IN (SELECT memory_id FROM erase_repo_memories)";
const DELETE_COMMIT_SIDECAR_SQL: &str = "DELETE FROM proxima_code.commit_v1
      WHERE memory_id IN (SELECT memory_id FROM erase_repo_memories)";
const DELETE_COMMIT_SUMMARY_SIDECAR_SQL: &str = "DELETE FROM proxima_code.commit_summary_v1
      WHERE memory_id IN (SELECT memory_id FROM erase_repo_memories)";
const DELETE_CODE_CHUNK_SIDECAR_SQL: &str = "DELETE FROM proxima_code.code_chunk_v1
      WHERE memory_id IN (SELECT memory_id FROM erase_repo_memories)";

// Call sites belong to the caller chunk, so they leave with the chunk row
// rather than with the index rows the chunk implied.
const DELETE_CODE_CHUNK_CALL_SIDECAR_SQL: &str = "DELETE FROM proxima_code.code_chunk_call_v1
  WHERE caller_memory_id IN (SELECT memory_id FROM erase_repo_memories)
     OR callee_memory_id IN (SELECT memory_id FROM erase_repo_memories)";
const DELETE_WORK_ASSIGNMENT_SIDECAR_SQL: &str = "DELETE FROM proxima_code.work_assignment_v1
  WHERE memory_id IN (SELECT memory_id FROM erase_repo_memories)
     OR work_item_memory_id IN (SELECT memory_id FROM erase_repo_memories)";
const DELETE_EMBEDDING_JOBS_SQL: &str = "DELETE FROM proxima_core.embedding_jobs
  WHERE entity_id IN (SELECT memory_id FROM erase_repo_memories)";
const DELETE_EMBEDDING_HEADS_SQL: &str = "DELETE FROM proxima_core.embedding_heads
  WHERE entity_id IN (SELECT memory_id FROM erase_repo_memories)";
const DELETE_EMBEDDINGS_SQL: &str = "DELETE FROM proxima_core.embeddings
  WHERE entity_id IN (SELECT memory_id FROM erase_repo_memories)";
const DELETE_MCP_CALL_SIDECAR_SQL: &str = "DELETE FROM proxima_core.mcp_call_logged_v1
  WHERE memory_id IN (SELECT memory_id FROM erase_repo_memories)";
const DELETE_AGENT_DERIVATION_SIDECAR_SQL: &str = "DELETE FROM proxima_core.agent_derivation_v1
  WHERE memory_id IN (SELECT memory_id FROM erase_repo_memories)";
const DELETE_AGENT_NOTE_SIDECAR_SQL: &str = "DELETE FROM proxima_core.agent_note_v1
  WHERE memory_id IN (SELECT memory_id FROM erase_repo_memories)";
const DELETE_SELECTED_EDGES_SQL: &str = "DELETE FROM proxima_core.edges e
  USING erase_repo_edges s
  WHERE e.source_kind = s.source_kind AND e.source_id = s.source_id
    AND e.target_kind = s.target_kind AND e.target_id = s.target_id
    AND e.kind = s.kind";
const DELETE_CHANGE_EVENTS_SQL: &str = "DELETE FROM proxima_core.change_event
  WHERE entity_memory_id IN (SELECT memory_id FROM erase_repo_memories)
     OR supersedes_memory_id IN (SELECT memory_id FROM erase_repo_memories)
     OR edge_source_id IN (SELECT memory_id FROM erase_repo_memories)
     OR edge_target_id IN (SELECT memory_id FROM erase_repo_memories)";
const DELETE_CITED_UPLOADS_SQL: &str = "DELETE FROM proxima_core.cited_object_uploads
  WHERE cited_object_id IN (SELECT cited_object_id FROM erase_repo_cited_objects)";
const DELETE_CITED_UPLOADED_BLOB_SQL: &str = "DELETE FROM proxima_core.cited_uploaded_blob_v1
  WHERE cited_object_id IN (SELECT cited_object_id FROM erase_repo_cited_objects)";
const DELETE_CITED_MCP_IO_SQL: &str = "DELETE FROM proxima_core.cited_mcp_call_io_v1
  WHERE cited_object_id IN (SELECT cited_object_id FROM erase_repo_cited_objects)";
const DELETE_CITATION_MAPPINGS_SQL: &str = "DELETE FROM proxima_core.citation_mappings
  WHERE citation_mapping_id IN (SELECT citation_mapping_id FROM erase_repo_citation_mappings)";
const DELETE_CITED_OBJECTS_SQL: &str = "DELETE FROM proxima_core.cited_objects
  WHERE cited_object_id IN (SELECT cited_object_id FROM erase_repo_cited_objects)";
const DELETE_FACT_ENTITIES_SQL: &str = "DELETE FROM proxima_core.fact_entities
  WHERE fact_entity_id IN (SELECT fact_entity_id FROM erase_repo_fact_entities)";
const DELETE_SELECTED_MEMORIES_SQL: &str = "DELETE FROM proxima_core.memories
  WHERE memory_id IN (SELECT memory_id FROM erase_repo_memories)";
const DELETE_FACT_RECEIPTS_SQL: &str = "DELETE FROM proxima_core.fact_receipts
  WHERE receipt_id IN (SELECT receipt_id FROM erase_repo_receipts)";
const DELETE_SOURCE_BATCHES_SQL: &str = "DELETE FROM proxima_core.source_batches
  WHERE id IN (SELECT id FROM erase_repo_source_batches)";
