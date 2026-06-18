use proxima_core::{MemoryId, StorageError};
use proxima_storage_pg::verbs::event_ingest::{EventIngestSidecarFuture, PgFactSidecar};
use sqlx::{Postgres, Transaction};

use crate::payloads::{
    AcceptanceCriteriaV1, AcceptanceVerificationV1, CommitV1, ExecutionRequestV1,
    ExecutionResultV1, FileRevisionV1, TestRequestV1, TestResultV1,
};

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
            let criteria_json = serde_json::to_value(&self.criteria)
                .map_err(|err| StorageError::Internal(format!("serialize criteria: {err}")))?;
            sqlx::query(
                "INSERT INTO proxima_code.acceptance_criteria_v1
                    (memory_id, work_item_memory_id, criteria_json)
                 VALUES ($1, $2, $3)",
            )
            .bind(memory_id.into_inner())
            .bind(self.work_item_memory_id)
            .bind(criteria_json)
            .execute(tx.as_mut())
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
            Ok(())
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
            let criteria_json = serde_json::to_value(&self.criteria)
                .map_err(|err| StorageError::Internal(format!("serialize criteria: {err}")))?;
            sqlx::query(
                "INSERT INTO proxima_code.test_requested_v1
                    (memory_id, repo_id, title, instructions, test_key, criteria_json)
                 VALUES ($1, $2, $3, $4, $5, $6)",
            )
            .bind(memory_id.into_inner())
            .bind(self.repo_id)
            .bind(&self.title)
            .bind(&self.instructions)
            .bind(&self.test_key)
            .bind(criteria_json)
            .execute(tx.as_mut())
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
            Ok(())
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
            let artifact_refs = serde_json::to_value(&self.artifact_refs)
                .map_err(|err| StorageError::Internal(format!("serialize artifacts: {err}")))?;
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
            .bind(artifact_refs)
            .bind(self.log_excerpt.as_deref())
            .execute(tx.as_mut())
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
            Ok(())
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
            let artifact_refs = serde_json::to_value(&self.artifact_refs)
                .map_err(|err| StorageError::Internal(format!("serialize artifacts: {err}")))?;
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
            .bind(artifact_refs)
            .bind(self.log_excerpt.as_deref())
            .execute(tx.as_mut())
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
            Ok(())
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
            let artifact_refs = serde_json::to_value(&self.artifact_refs)
                .map_err(|err| StorageError::Internal(format!("serialize artifacts: {err}")))?;
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
            .bind(artifact_refs)
            .bind(self.verifier_memory_id)
            .execute(tx.as_mut())
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
            Ok(())
        })
    }
}
