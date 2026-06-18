use proxima_core::{EdgeId, MemoryId, SidecarPayload, StorageError};
use proxima_storage_pg::sidecars::{
    PgEdgeSidecar, PgMemoryPayload, PgMemoryPayloadFuture, PgMemorySidecar, PgSidecarFuture,
};
use proxima_storage_pg::verbs::event_ingest::{EventIngestSidecarFuture, PgFactSidecar};
use sqlx::{PgPool, Postgres, Transaction};

use crate::payloads::{
    AcceptanceCriteriaV1, AcceptanceCriterionV1, AcceptanceSummaryV1, AcceptanceVerificationV1,
    AcceptanceVerifierKind, AcceptanceVerifierSpecV1, CodeChunkV1, CodeCommitSummarizerSelfV1,
    CodeDevelopmentPerspectiveV1, CodeEngineerSelfV1, CodeExecutionPlanItemKind,
    CodeExecutionPlanItemV1, CodeExecutionPlanV1, CommitSummaryV1, CommitV1, EdgeCallsV1,
    ExecutionRequestV1, ExecutionResultV1, FileRevisionV1, FileState, TestRequestV1, TestResultV1,
};

fn int_to_u32(value: i64, column: &str) -> Result<u32, StorageError> {
    u32::try_from(value).map_err(|err| StorageError::Internal(format!("invalid {column}: {err}")))
}

fn int_to_u64(value: i64, column: &str) -> Result<u64, StorageError> {
    u64::try_from(value).map_err(|err| StorageError::Internal(format!("invalid {column}: {err}")))
}

fn bytes32(bytes: &[u8], column: &str) -> Result<[u8; 32], StorageError> {
    <[u8; 32]>::try_from(bytes).map_err(|_| {
        StorageError::Internal(format!(
            "{column} must be exactly 32 bytes, got {}",
            bytes.len()
        ))
    })
}

async fn insert_criteria_rows(
    tx: &mut Transaction<'_, Postgres>,
    table: &'static str,
    parent_column: &'static str,
    parent_id: MemoryId,
    criteria: &[AcceptanceCriterionV1],
) -> Result<(), StorageError> {
    let sql = format!(
        "INSERT INTO {table}
            ({parent_column}, criterion_index, criterion_key, description,
             required, verifier_kind, verifier_path, verifier_command,
             verifier_pattern, verifier_note)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)"
    );
    for (index, criterion) in criteria.iter().enumerate() {
        sqlx::query(&sql)
            .bind(parent_id.into_inner())
            .bind(i32::try_from(index).unwrap_or(i32::MAX))
            .bind(&criterion.key)
            .bind(&criterion.description)
            .bind(criterion.required)
            .bind(criterion.verifier_kind)
            .bind(criterion.verifier_spec.path.as_deref())
            .bind(&criterion.verifier_spec.command)
            .bind(criterion.verifier_spec.pattern.as_deref())
            .bind(criterion.verifier_spec.note.as_deref())
            .execute(tx.as_mut())
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
    }
    Ok(())
}

#[derive(Debug, sqlx::FromRow)]
struct CriterionPayloadRow {
    criterion_key: String,
    description: String,
    required: bool,
    verifier_kind: AcceptanceVerifierKind,
    verifier_path: Option<String>,
    verifier_command: Option<Vec<String>>,
    verifier_pattern: Option<String>,
    verifier_note: Option<String>,
}

#[derive(Debug, sqlx::FromRow)]
struct CommitPayloadRow {
    repo_id: uuid::Uuid,
    sha: String,
    parents: Vec<String>,
    author_name: String,
    author_email: String,
    author_time: time::OffsetDateTime,
    committer_name: String,
    committer_email: String,
    committer_time: time::OffsetDateTime,
    message: String,
}

#[derive(Debug, sqlx::FromRow)]
struct FileRevisionPayloadRow {
    repo_id: uuid::Uuid,
    file_path: String,
    language: Option<String>,
    content_sha256: Vec<u8>,
    size_bytes: i64,
    indexed_commit_sha: String,
    state: FileState,
}

#[derive(Debug, sqlx::FromRow)]
struct WorkResultPayloadRow {
    related_memory_id: uuid::Uuid,
    repo_id: uuid::Uuid,
    status: crate::payloads::WorkResultStatus,
    summary: String,
    artifact_refs: Vec<String>,
    log_excerpt: Option<String>,
}

#[derive(Debug, sqlx::FromRow)]
struct AcceptanceVerificationPayloadRow {
    work_item_memory_id: uuid::Uuid,
    criterion_key: String,
    status: crate::payloads::AcceptanceVerificationStatus,
    summary: String,
    artifact_refs: Vec<String>,
    verifier_memory_id: Option<uuid::Uuid>,
}

#[derive(Debug, sqlx::FromRow)]
struct CodeChunkPayloadRow {
    repo_id: uuid::Uuid,
    file_path: String,
    chunk_index: i32,
    text: String,
    language: Option<String>,
    chunk_type: String,
    byte_range_start: i64,
    byte_range_end: i64,
    line_range_start: i64,
    line_range_end: i64,
    state: FileState,
}

async fn load_criteria_rows(
    pool: &PgPool,
    table: &'static str,
    parent_column: &'static str,
    parent_id: MemoryId,
) -> Result<Vec<AcceptanceCriterionV1>, StorageError> {
    let sql = format!(
        "SELECT criterion_key, description, required, verifier_kind,
                verifier_path, verifier_command, verifier_pattern, verifier_note
           FROM {table}
          WHERE {parent_column} = $1
          ORDER BY criterion_index ASC"
    );
    let rows: Vec<CriterionPayloadRow> = sqlx::query_as(&sql)
        .bind(parent_id.into_inner())
        .fetch_all(pool)
        .await
        .map_err(|err| StorageError::Internal(err.to_string()))?;
    Ok(rows
        .into_iter()
        .map(|row| AcceptanceCriterionV1 {
            key: row.criterion_key,
            description: row.description,
            required: row.required,
            verifier_kind: row.verifier_kind,
            verifier_spec: AcceptanceVerifierSpecV1 {
                path: row.verifier_path,
                command: row.verifier_command,
                pattern: row.verifier_pattern,
                note: row.verifier_note,
            },
        })
        .collect())
}

impl PgEdgeSidecar for EdgeCallsV1 {
    fn insert_edge_sidecar<'t>(
        &'t self,
        tx: &'t mut sqlx::PgConnection,
        edge_id: EdgeId,
    ) -> PgSidecarFuture<'t> {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO proxima_code.code_calls_v1
                    (edge_id, callsite_byte_start, callsite_byte_end, callee_name, is_dynamic)
                 VALUES ($1, $2, $3, $4, $5)",
            )
            .bind(edge_id.into_inner())
            .bind(i32::try_from(self.callsite_byte_start).unwrap_or(i32::MAX))
            .bind(i32::try_from(self.callsite_byte_end).unwrap_or(i32::MAX))
            .bind(&self.callee_name)
            .bind(self.is_dynamic)
            .execute(tx)
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
            Ok(())
        })
    }
}

impl PgFactSidecar for CommitV1 {
    fn insert_sidecar<'t>(
        self,
        tx: &'t mut Transaction<'_, Postgres>,
        memory_id: MemoryId,
    ) -> EventIngestSidecarFuture<'t>
    where
        Self: 't,
    {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO proxima_code.commit_v1 \
                    (memory_id, repo_id, sha, parents, author_name, author_email, \
                     author_time, committer_name, committer_email, committer_time, \
                     message) \
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
            )
            .bind(memory_id.into_inner())
            .bind(self.repo_id)
            .bind(&self.sha)
            .bind(&self.parents)
            .bind(&self.author_name)
            .bind(&self.author_email)
            .bind(self.author_time)
            .bind(&self.committer_name)
            .bind(&self.committer_email)
            .bind(self.committer_time)
            .bind(&self.message)
            .execute(tx.as_mut())
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
            Ok(())
        })
    }
}

impl PgMemoryPayload for CommitV1 {
    fn load_memory_payload(pool: &PgPool, memory_id: MemoryId) -> PgMemoryPayloadFuture<'_> {
        Box::pin(async move {
            let row: Option<CommitPayloadRow> = sqlx::query_as(
                "SELECT repo_id, sha, parents, author_name, author_email,
                        author_time, committer_name, committer_email,
                        committer_time, message
                   FROM proxima_code.commit_v1
                  WHERE memory_id = $1",
            )
            .bind(memory_id.into_inner())
            .fetch_optional(pool)
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
            Ok(row.map(|row| {
                SidecarPayload::fact(CommitV1 {
                    repo_id: row.repo_id,
                    sha: row.sha,
                    parents: row.parents,
                    author_name: row.author_name,
                    author_email: row.author_email,
                    author_time: row.author_time,
                    committer_name: row.committer_name,
                    committer_email: row.committer_email,
                    committer_time: row.committer_time,
                    message: row.message,
                })
            }))
        })
    }
}

impl PgFactSidecar for FileRevisionV1 {
    fn insert_sidecar<'t>(
        self,
        tx: &'t mut Transaction<'_, Postgres>,
        memory_id: MemoryId,
    ) -> EventIngestSidecarFuture<'t>
    where
        Self: 't,
    {
        Box::pin(async move {
            let size_bytes = i64::try_from(self.size_bytes).unwrap_or(i64::MAX);
            sqlx::query(
                "INSERT INTO proxima_code.file_revision_v1 \
                    (memory_id, repo_id, file_path, language, content_sha256, \
                     size_bytes, indexed_commit_sha, state) \
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
            )
            .bind(memory_id.into_inner())
            .bind(self.repo_id)
            .bind(&self.file_path)
            .bind(self.language.as_deref())
            .bind(self.content_sha256.to_vec())
            .bind(size_bytes)
            .bind(&self.indexed_commit_sha)
            .bind(self.state)
            .execute(tx.as_mut())
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
            Ok(())
        })
    }
}

impl PgMemoryPayload for FileRevisionV1 {
    fn load_memory_payload(pool: &PgPool, memory_id: MemoryId) -> PgMemoryPayloadFuture<'_> {
        Box::pin(async move {
            let row: Option<FileRevisionPayloadRow> = sqlx::query_as(
                "SELECT repo_id, file_path, language, content_sha256,
                        size_bytes, indexed_commit_sha, state
                   FROM proxima_code.file_revision_v1
                  WHERE memory_id = $1",
            )
            .bind(memory_id.into_inner())
            .fetch_optional(pool)
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
            row.map(|row| {
                Ok(SidecarPayload::fact(FileRevisionV1 {
                    repo_id: row.repo_id,
                    file_path: row.file_path,
                    language: row.language,
                    content_sha256: bytes32(&row.content_sha256, "content_sha256")?,
                    size_bytes: int_to_u64(row.size_bytes, "size_bytes")?,
                    indexed_commit_sha: row.indexed_commit_sha,
                    state: row.state,
                }))
            })
            .transpose()
        })
    }
}

impl PgFactSidecar for ExecutionRequestV1 {
    fn insert_sidecar<'t>(
        self,
        tx: &'t mut Transaction<'_, Postgres>,
        memory_id: MemoryId,
    ) -> EventIngestSidecarFuture<'t>
    where
        Self: 't,
    {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO proxima_code.work_requested_v1
                    (memory_id, repo_id, title, instructions, request_key)
                 VALUES ($1, $2, $3, $4, $5)",
            )
            .bind(memory_id.into_inner())
            .bind(self.repo_id)
            .bind(&self.title)
            .bind(&self.instructions)
            .bind(&self.request_key)
            .execute(tx.as_mut())
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
            Ok(())
        })
    }
}

impl PgMemoryPayload for ExecutionRequestV1 {
    fn load_memory_payload(pool: &PgPool, memory_id: MemoryId) -> PgMemoryPayloadFuture<'_> {
        Box::pin(async move {
            let row: Option<(uuid::Uuid, String, String, String)> = sqlx::query_as(
                "SELECT repo_id, title, instructions, request_key
                   FROM proxima_code.work_requested_v1
                  WHERE memory_id = $1",
            )
            .bind(memory_id.into_inner())
            .fetch_optional(pool)
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
            Ok(row.map(|(repo_id, title, instructions, request_key)| {
                SidecarPayload::fact(ExecutionRequestV1 {
                    repo_id,
                    title,
                    instructions,
                    request_key,
                })
            }))
        })
    }
}

impl PgFactSidecar for AcceptanceCriteriaV1 {
    fn insert_sidecar<'t>(
        self,
        tx: &'t mut Transaction<'_, Postgres>,
        memory_id: MemoryId,
    ) -> EventIngestSidecarFuture<'t>
    where
        Self: 't,
    {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO proxima_code.acceptance_criteria_v1
                    (memory_id, work_item_memory_id, criteria_count)
                 VALUES ($1, $2, $3)",
            )
            .bind(memory_id.into_inner())
            .bind(self.work_item_memory_id)
            .bind(i32::try_from(self.criteria.len()).unwrap_or(i32::MAX))
            .execute(tx.as_mut())
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
            insert_criteria_rows(
                tx,
                "proxima_code.acceptance_criterion_v1",
                "criteria_memory_id",
                memory_id,
                &self.criteria,
            )
            .await?;
            Ok(())
        })
    }
}

impl PgMemoryPayload for AcceptanceCriteriaV1 {
    fn load_memory_payload(pool: &PgPool, memory_id: MemoryId) -> PgMemoryPayloadFuture<'_> {
        Box::pin(async move {
            let work_item_memory_id: Option<uuid::Uuid> = sqlx::query_scalar(
                "SELECT work_item_memory_id
                   FROM proxima_code.acceptance_criteria_v1
                  WHERE memory_id = $1",
            )
            .bind(memory_id.into_inner())
            .fetch_optional(pool)
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
            let Some(work_item_memory_id) = work_item_memory_id else {
                return Ok(None);
            };
            let criteria = load_criteria_rows(
                pool,
                "proxima_code.acceptance_criterion_v1",
                "criteria_memory_id",
                memory_id,
            )
            .await?;
            Ok(Some(SidecarPayload::fact(AcceptanceCriteriaV1 {
                work_item_memory_id,
                criteria,
            })))
        })
    }
}

impl PgFactSidecar for TestRequestV1 {
    fn insert_sidecar<'t>(
        self,
        tx: &'t mut Transaction<'_, Postgres>,
        memory_id: MemoryId,
    ) -> EventIngestSidecarFuture<'t>
    where
        Self: 't,
    {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO proxima_code.test_requested_v1
                    (memory_id, repo_id, title, instructions, test_key, criteria_count)
                 VALUES ($1, $2, $3, $4, $5, $6)",
            )
            .bind(memory_id.into_inner())
            .bind(self.repo_id)
            .bind(&self.title)
            .bind(&self.instructions)
            .bind(&self.test_key)
            .bind(i32::try_from(self.criteria.len()).unwrap_or(i32::MAX))
            .execute(tx.as_mut())
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
            insert_criteria_rows(
                tx,
                "proxima_code.test_requested_criterion_v1",
                "test_requested_memory_id",
                memory_id,
                &self.criteria,
            )
            .await?;
            Ok(())
        })
    }
}

impl PgMemoryPayload for TestRequestV1 {
    fn load_memory_payload(pool: &PgPool, memory_id: MemoryId) -> PgMemoryPayloadFuture<'_> {
        Box::pin(async move {
            let row: Option<(uuid::Uuid, String, String, String)> = sqlx::query_as(
                "SELECT repo_id, title, instructions, test_key
                   FROM proxima_code.test_requested_v1
                  WHERE memory_id = $1",
            )
            .bind(memory_id.into_inner())
            .fetch_optional(pool)
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
            let Some((repo_id, title, instructions, test_key)) = row else {
                return Ok(None);
            };
            let criteria = load_criteria_rows(
                pool,
                "proxima_code.test_requested_criterion_v1",
                "test_requested_memory_id",
                memory_id,
            )
            .await?;
            Ok(Some(SidecarPayload::fact(TestRequestV1 {
                repo_id,
                title,
                instructions,
                test_key,
                criteria,
            })))
        })
    }
}

impl PgFactSidecar for ExecutionResultV1 {
    fn insert_sidecar<'t>(
        self,
        tx: &'t mut Transaction<'_, Postgres>,
        memory_id: MemoryId,
    ) -> EventIngestSidecarFuture<'t>
    where
        Self: 't,
    {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO proxima_code.execution_result_v1
                    (memory_id, work_requested_memory_id, repo_id, status, summary, artifact_refs, log_excerpt)
                 VALUES ($1, $2, $3, $4, $5, $6, $7)",
            )
            .bind(memory_id.into_inner())
            .bind(self.work_requested_memory_id)
            .bind(self.repo_id)
            .bind(self.status)
            .bind(&self.summary)
            .bind(&self.artifact_refs)
            .bind(self.log_excerpt.as_deref())
            .execute(tx.as_mut())
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
            Ok(())
        })
    }
}

impl PgMemoryPayload for ExecutionResultV1 {
    fn load_memory_payload(pool: &PgPool, memory_id: MemoryId) -> PgMemoryPayloadFuture<'_> {
        Box::pin(async move {
            let row: Option<WorkResultPayloadRow> = sqlx::query_as(
                "SELECT work_requested_memory_id AS related_memory_id, repo_id, status,
                        summary, artifact_refs, log_excerpt
                   FROM proxima_code.execution_result_v1
                  WHERE memory_id = $1",
            )
            .bind(memory_id.into_inner())
            .fetch_optional(pool)
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
            Ok(row.map(|row| {
                SidecarPayload::fact(ExecutionResultV1 {
                    work_requested_memory_id: row.related_memory_id,
                    repo_id: row.repo_id,
                    status: row.status,
                    summary: row.summary,
                    artifact_refs: row.artifact_refs,
                    log_excerpt: row.log_excerpt,
                })
            }))
        })
    }
}

impl PgFactSidecar for TestResultV1 {
    fn insert_sidecar<'t>(
        self,
        tx: &'t mut Transaction<'_, Postgres>,
        memory_id: MemoryId,
    ) -> EventIngestSidecarFuture<'t>
    where
        Self: 't,
    {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO proxima_code.test_result_v1
                    (memory_id, test_requested_memory_id, repo_id, status, summary, artifact_refs, log_excerpt)
                 VALUES ($1, $2, $3, $4, $5, $6, $7)",
            )
            .bind(memory_id.into_inner())
            .bind(self.test_requested_memory_id)
            .bind(self.repo_id)
            .bind(self.status)
            .bind(&self.summary)
            .bind(&self.artifact_refs)
            .bind(self.log_excerpt.as_deref())
            .execute(tx.as_mut())
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
            Ok(())
        })
    }
}

impl PgMemoryPayload for TestResultV1 {
    fn load_memory_payload(pool: &PgPool, memory_id: MemoryId) -> PgMemoryPayloadFuture<'_> {
        Box::pin(async move {
            let row: Option<WorkResultPayloadRow> = sqlx::query_as(
                "SELECT test_requested_memory_id AS related_memory_id, repo_id, status,
                        summary, artifact_refs, log_excerpt
                   FROM proxima_code.test_result_v1
                  WHERE memory_id = $1",
            )
            .bind(memory_id.into_inner())
            .fetch_optional(pool)
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
            Ok(row.map(|row| {
                SidecarPayload::fact(TestResultV1 {
                    test_requested_memory_id: row.related_memory_id,
                    repo_id: row.repo_id,
                    status: row.status,
                    summary: row.summary,
                    artifact_refs: row.artifact_refs,
                    log_excerpt: row.log_excerpt,
                })
            }))
        })
    }
}

impl PgFactSidecar for AcceptanceVerificationV1 {
    fn insert_sidecar<'t>(
        self,
        tx: &'t mut Transaction<'_, Postgres>,
        memory_id: MemoryId,
    ) -> EventIngestSidecarFuture<'t>
    where
        Self: 't,
    {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO proxima_code.acceptance_verification_v1
                    (memory_id, work_item_memory_id, criterion_key, status, summary, artifact_refs, verifier_memory_id)
                 VALUES ($1, $2, $3, $4, $5, $6, $7)",
            )
            .bind(memory_id.into_inner())
            .bind(self.work_item_memory_id)
            .bind(&self.criterion_key)
            .bind(self.status)
            .bind(&self.summary)
            .bind(&self.artifact_refs)
            .bind(self.verifier_memory_id)
            .execute(tx.as_mut())
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
            Ok(())
        })
    }
}

impl PgMemoryPayload for AcceptanceVerificationV1 {
    fn load_memory_payload(pool: &PgPool, memory_id: MemoryId) -> PgMemoryPayloadFuture<'_> {
        Box::pin(async move {
            let row: Option<AcceptanceVerificationPayloadRow> = sqlx::query_as(
                "SELECT work_item_memory_id, criterion_key, status,
                        summary, artifact_refs, verifier_memory_id
                   FROM proxima_code.acceptance_verification_v1
                  WHERE memory_id = $1",
            )
            .bind(memory_id.into_inner())
            .fetch_optional(pool)
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
            Ok(row.map(|row| {
                SidecarPayload::fact(AcceptanceVerificationV1 {
                    work_item_memory_id: row.work_item_memory_id,
                    criterion_key: row.criterion_key,
                    status: row.status,
                    summary: row.summary,
                    artifact_refs: row.artifact_refs,
                    verifier_memory_id: row.verifier_memory_id,
                })
            }))
        })
    }
}

impl PgMemorySidecar for CodeChunkV1 {
    fn insert_memory_sidecar<'t>(
        &'t self,
        tx: &'t mut Transaction<'_, Postgres>,
        memory_id: MemoryId,
    ) -> PgSidecarFuture<'t> {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO proxima_code.code_chunk_v1
                    (memory_id, repo_id, file_path, chunk_index, text, language,
                     chunk_type, byte_range_start, byte_range_end,
                     line_range_start, line_range_end, state)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
            )
            .bind(memory_id.into_inner())
            .bind(self.repo_id)
            .bind(&self.file_path)
            .bind(i32::try_from(self.chunk_index).unwrap_or(i32::MAX))
            .bind(&self.text)
            .bind(self.language.as_deref())
            .bind(&self.chunk_type)
            .bind(i64::from(self.byte_range_start))
            .bind(i64::from(self.byte_range_end))
            .bind(i64::from(self.line_range_start))
            .bind(i64::from(self.line_range_end))
            .bind(self.state)
            .execute(tx.as_mut())
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
            Ok(())
        })
    }
}

impl PgMemoryPayload for CodeChunkV1 {
    fn load_memory_payload(pool: &PgPool, memory_id: MemoryId) -> PgMemoryPayloadFuture<'_> {
        Box::pin(async move {
            let row: Option<CodeChunkPayloadRow> = sqlx::query_as(
                "SELECT repo_id, file_path, chunk_index, text, language,
                        chunk_type, byte_range_start, byte_range_end,
                        line_range_start, line_range_end, state
                   FROM proxima_code.code_chunk_v1
                  WHERE memory_id = $1",
            )
            .bind(memory_id.into_inner())
            .fetch_optional(pool)
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
            row.map(|row| {
                Ok(SidecarPayload::abstraction(CodeChunkV1 {
                    repo_id: row.repo_id,
                    file_path: row.file_path,
                    chunk_index: u32::try_from(row.chunk_index).map_err(|err| {
                        StorageError::Internal(format!("invalid chunk_index: {err}"))
                    })?,
                    text: row.text,
                    language: row.language,
                    chunk_type: row.chunk_type,
                    byte_range_start: int_to_u32(row.byte_range_start, "byte_range_start")?,
                    byte_range_end: int_to_u32(row.byte_range_end, "byte_range_end")?,
                    line_range_start: int_to_u32(row.line_range_start, "line_range_start")?,
                    line_range_end: int_to_u32(row.line_range_end, "line_range_end")?,
                    state: row.state,
                }))
            })
            .transpose()
        })
    }
}

impl PgMemorySidecar for CommitSummaryV1 {
    fn insert_memory_sidecar<'t>(
        &'t self,
        tx: &'t mut Transaction<'_, Postgres>,
        memory_id: MemoryId,
    ) -> PgSidecarFuture<'t> {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO proxima_code.commit_summary_v1
                    (memory_id, repo_id, commit_sha, summary, key_files, change_kind)
                 VALUES ($1, $2, $3, $4, $5, $6)",
            )
            .bind(memory_id.into_inner())
            .bind(self.repo_id)
            .bind(&self.commit_sha)
            .bind(&self.summary)
            .bind(&self.key_files)
            .bind(&self.change_kind)
            .execute(tx.as_mut())
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
            Ok(())
        })
    }
}

impl PgMemoryPayload for CommitSummaryV1 {
    fn load_memory_payload(pool: &PgPool, memory_id: MemoryId) -> PgMemoryPayloadFuture<'_> {
        Box::pin(async move {
            let row: Option<(uuid::Uuid, String, String, Vec<String>, String)> = sqlx::query_as(
                "SELECT repo_id, commit_sha, summary, key_files, change_kind
                   FROM proxima_code.commit_summary_v1
                  WHERE memory_id = $1",
            )
            .bind(memory_id.into_inner())
            .fetch_optional(pool)
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
            Ok(
                row.map(|(repo_id, commit_sha, summary, key_files, change_kind)| {
                    SidecarPayload::abstraction(CommitSummaryV1 {
                        repo_id,
                        commit_sha,
                        summary,
                        key_files,
                        change_kind,
                    })
                }),
            )
        })
    }
}

impl PgMemorySidecar for CodeExecutionPlanV1 {
    fn insert_memory_sidecar<'t>(
        &'t self,
        tx: &'t mut Transaction<'_, Postgres>,
        memory_id: MemoryId,
    ) -> PgSidecarFuture<'t> {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO proxima_code.execution_plan_v1
                    (memory_id, repo_id, plan_key, goal_activated_memory_id,
                     summary, item_count, evidence_memory_ids)
                 VALUES ($1, $2, $3, $4, $5, $6, $7)",
            )
            .bind(memory_id.into_inner())
            .bind(self.repo_id)
            .bind(&self.plan_key)
            .bind(self.goal_activated_memory_id)
            .bind(&self.summary)
            .bind(i32::try_from(self.items.len()).unwrap_or(i32::MAX))
            .bind(&self.evidence_memory_ids)
            .execute(tx.as_mut())
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
            for (index, item) in self.items.iter().enumerate() {
                sqlx::query(
                    "INSERT INTO proxima_code.execution_plan_item_v1
                        (plan_memory_id, item_index, item_key, kind,
                         title, depends_on, request_key)
                     VALUES ($1, $2, $3, $4, $5, $6, $7)",
                )
                .bind(memory_id.into_inner())
                .bind(i32::try_from(index).unwrap_or(i32::MAX))
                .bind(&item.key)
                .bind(item.kind)
                .bind(&item.title)
                .bind(&item.depends_on)
                .bind(&item.request_key)
                .execute(tx.as_mut())
                .await
                .map_err(|err| StorageError::Internal(err.to_string()))?;
            }
            Ok(())
        })
    }
}

#[derive(Debug, sqlx::FromRow)]
struct ExecutionPlanItemPayloadRow {
    item_key: String,
    kind: CodeExecutionPlanItemKind,
    title: String,
    depends_on: Vec<String>,
    request_key: String,
}

impl PgMemoryPayload for CodeExecutionPlanV1 {
    fn load_memory_payload(pool: &PgPool, memory_id: MemoryId) -> PgMemoryPayloadFuture<'_> {
        Box::pin(async move {
            let row: Option<(uuid::Uuid, String, uuid::Uuid, String, Vec<uuid::Uuid>)> =
                sqlx::query_as(
                    "SELECT repo_id, plan_key, goal_activated_memory_id,
                            summary, evidence_memory_ids
                       FROM proxima_code.execution_plan_v1
                      WHERE memory_id = $1",
                )
                .bind(memory_id.into_inner())
                .fetch_optional(pool)
                .await
                .map_err(|err| StorageError::Internal(err.to_string()))?;
            let Some((repo_id, plan_key, goal_activated_memory_id, summary, evidence_memory_ids)) =
                row
            else {
                return Ok(None);
            };
            let item_rows: Vec<ExecutionPlanItemPayloadRow> = sqlx::query_as(
                "SELECT item_key, kind, title, depends_on, request_key
                   FROM proxima_code.execution_plan_item_v1
                  WHERE plan_memory_id = $1
                  ORDER BY item_index ASC",
            )
            .bind(memory_id.into_inner())
            .fetch_all(pool)
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
            let items = item_rows
                .into_iter()
                .map(|row| CodeExecutionPlanItemV1 {
                    key: row.item_key,
                    kind: row.kind,
                    title: row.title,
                    depends_on: row.depends_on,
                    request_key: row.request_key,
                })
                .collect();
            Ok(Some(SidecarPayload::abstraction(CodeExecutionPlanV1 {
                repo_id,
                plan_key,
                goal_activated_memory_id,
                summary,
                items,
                evidence_memory_ids,
            })))
        })
    }
}

impl PgMemorySidecar for AcceptanceSummaryV1 {
    fn insert_memory_sidecar<'t>(
        &'t self,
        tx: &'t mut Transaction<'_, Postgres>,
        memory_id: MemoryId,
    ) -> PgSidecarFuture<'t> {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO proxima_code.acceptance_summary_v1
                    (memory_id, work_item_memory_id, repo_id, passed_required,
                     summary, verification_memory_ids)
                 VALUES ($1, $2, $3, $4, $5, $6)",
            )
            .bind(memory_id.into_inner())
            .bind(self.work_item_memory_id)
            .bind(self.repo_id)
            .bind(self.passed_required)
            .bind(&self.summary)
            .bind(&self.verification_memory_ids)
            .execute(tx.as_mut())
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
            Ok(())
        })
    }
}

impl PgMemoryPayload for AcceptanceSummaryV1 {
    fn load_memory_payload(pool: &PgPool, memory_id: MemoryId) -> PgMemoryPayloadFuture<'_> {
        Box::pin(async move {
            let row: Option<(uuid::Uuid, uuid::Uuid, bool, String, Vec<uuid::Uuid>)> =
                sqlx::query_as(
                    "SELECT work_item_memory_id, repo_id, passed_required,
                            summary, verification_memory_ids
                       FROM proxima_code.acceptance_summary_v1
                      WHERE memory_id = $1",
                )
                .bind(memory_id.into_inner())
                .fetch_optional(pool)
                .await
                .map_err(|err| StorageError::Internal(err.to_string()))?;
            Ok(row.map(
                |(
                    work_item_memory_id,
                    repo_id,
                    passed_required,
                    summary,
                    verification_memory_ids,
                )| {
                    SidecarPayload::abstraction(AcceptanceSummaryV1 {
                        work_item_memory_id,
                        repo_id,
                        passed_required,
                        summary,
                        verification_memory_ids,
                    })
                },
            ))
        })
    }
}

impl PgMemorySidecar for CodeDevelopmentPerspectiveV1 {
    fn insert_memory_sidecar<'t>(
        &'t self,
        tx: &'t mut Transaction<'_, Postgres>,
        memory_id: MemoryId,
    ) -> PgSidecarFuture<'t> {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO proxima_code.development_perspective_v1
                    (memory_id, repo_id, summary, pattern, risk,
                     recommended_posture, confidence)
                 VALUES ($1, $2, $3, $4, $5, $6, $7)",
            )
            .bind(memory_id.into_inner())
            .bind(self.repo_id)
            .bind(&self.summary)
            .bind(&self.pattern)
            .bind(&self.risk)
            .bind(&self.recommended_posture)
            .bind(self.confidence)
            .execute(tx.as_mut())
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
            Ok(())
        })
    }
}

impl PgMemoryPayload for CodeDevelopmentPerspectiveV1 {
    fn load_memory_payload(pool: &PgPool, memory_id: MemoryId) -> PgMemoryPayloadFuture<'_> {
        Box::pin(async move {
            let row: Option<(Option<uuid::Uuid>, String, String, String, String, f32)> =
                sqlx::query_as(
                    "SELECT repo_id, summary, pattern, risk,
                            recommended_posture, confidence
                       FROM proxima_code.development_perspective_v1
                      WHERE memory_id = $1",
                )
                .bind(memory_id.into_inner())
                .fetch_optional(pool)
                .await
                .map_err(|err| StorageError::Internal(err.to_string()))?;
            Ok(row.map(
                |(repo_id, summary, pattern, risk, recommended_posture, confidence)| {
                    SidecarPayload::perspective(CodeDevelopmentPerspectiveV1 {
                        repo_id,
                        summary,
                        pattern,
                        risk,
                        recommended_posture,
                        confidence,
                    })
                },
            ))
        })
    }
}

impl PgMemorySidecar for CodeCommitSummarizerSelfV1 {
    fn insert_memory_sidecar<'t>(
        &'t self,
        tx: &'t mut Transaction<'_, Postgres>,
        memory_id: MemoryId,
    ) -> PgSidecarFuture<'t> {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO proxima_code.commit_summarizer_self_v1
                    (memory_id, display_name, purpose)
                 VALUES ($1, $2, $3)",
            )
            .bind(memory_id.into_inner())
            .bind(&self.display_name)
            .bind(&self.purpose)
            .execute(tx.as_mut())
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
            Ok(())
        })
    }
}

impl PgMemoryPayload for CodeCommitSummarizerSelfV1 {
    fn load_memory_payload(pool: &PgPool, memory_id: MemoryId) -> PgMemoryPayloadFuture<'_> {
        Box::pin(async move {
            let row: Option<(String, String)> = sqlx::query_as(
                "SELECT display_name, purpose
                   FROM proxima_code.commit_summarizer_self_v1
                  WHERE memory_id = $1",
            )
            .bind(memory_id.into_inner())
            .fetch_optional(pool)
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
            Ok(row.map(|(display_name, purpose)| {
                SidecarPayload::perspective(CodeCommitSummarizerSelfV1 {
                    display_name,
                    purpose,
                })
            }))
        })
    }
}

impl PgMemorySidecar for CodeEngineerSelfV1 {
    fn insert_memory_sidecar<'t>(
        &'t self,
        tx: &'t mut Transaction<'_, Postgres>,
        memory_id: MemoryId,
    ) -> PgSidecarFuture<'t> {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO proxima_code.engineer_self_v1
                    (memory_id, display_name, purpose)
                 VALUES ($1, $2, $3)",
            )
            .bind(memory_id.into_inner())
            .bind(&self.display_name)
            .bind(&self.purpose)
            .execute(tx.as_mut())
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
            Ok(())
        })
    }
}

impl PgMemoryPayload for CodeEngineerSelfV1 {
    fn load_memory_payload(pool: &PgPool, memory_id: MemoryId) -> PgMemoryPayloadFuture<'_> {
        Box::pin(async move {
            let row: Option<(String, String)> = sqlx::query_as(
                "SELECT display_name, purpose
                   FROM proxima_code.engineer_self_v1
                  WHERE memory_id = $1",
            )
            .bind(memory_id.into_inner())
            .fetch_optional(pool)
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
            Ok(row.map(|(display_name, purpose)| {
                SidecarPayload::perspective(CodeEngineerSelfV1 {
                    display_name,
                    purpose,
                })
            }))
        })
    }
}
