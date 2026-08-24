use std::collections::BTreeMap;

use proxima_core::verbs::schema::PayloadKind;
use proxima_core::{MemoryId, SidecarPayload, StorageError};
use proxima_storage_pg::sidecars::{
    PgMemoryPayload, PgMemoryPayloadBatchFuture, PgMemoryPayloadFuture, PgMemorySidecar,
    PgSidecarFuture, PgSidecarReadCtx, SidecarInsertPermit,
};
use proxima_storage_pg::verbs::fact_ingest::{FactIngestSidecarFuture, PgFactSidecar};
use sqlx::{Postgres, Transaction};

use crate::payloads::{
    AcceptanceCriteriaV1, AcceptanceCriterionV1, AcceptanceSummaryV1, AcceptanceVerificationStatus,
    AcceptanceVerificationV1, AcceptanceVerifierKind, AcceptanceVerifierSpecV1, CodeCallSiteV1,
    CodeCallV1, CodeChunkV1, CodeCommitSummarizerSelfV1, CodeDevelopmentPerspectiveV1,
    CodeEngineerSelfV1, CodeExecutionPlanItemKind, CodeExecutionPlanItemV1, CodeExecutionPlanV1,
    CodeWorkAssignmentV1, CommitSummaryV1, CommitV1, ExecutionRequestV1, ExecutionResultV1,
    FileRevisionV1, FileState, TestRequestV1, TestResultV1, WorkResultStatus,
};

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
        // SQL-POLICY: fixed-fragment — `sql` is the format! above over a
        // caller-fixed table name; every criterion field is bound.
        sqlx::query(sqlx::AssertSqlSafe(sql.as_str()))
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
            .map_err(proxima_storage_pg::map_err)?;
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

async fn load_criteria_rows(
    ctx: PgSidecarReadCtx<'_>,
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
    let rows: Vec<CriterionPayloadRow> = ctx.fetch_all_by_memory_id(&sql, parent_id).await?;
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

fn file_state_to_str(value: FileState) -> &'static str {
    value.as_str()
}

fn parse_file_state(value: &str) -> Result<FileState, StorageError> {
    match value {
        "Present" => Ok(FileState::Present),
        "Tombstone" => Ok(FileState::Tombstone),
        other => Err(StorageError::Internal(format!(
            "invalid file state {other}"
        ))),
    }
}

fn work_result_status_to_str(value: WorkResultStatus) -> &'static str {
    value.as_str()
}

fn parse_work_result_status(value: &str) -> Result<WorkResultStatus, StorageError> {
    match value {
        "succeeded" => Ok(WorkResultStatus::Succeeded),
        "failed" => Ok(WorkResultStatus::Failed),
        "blocked" => Ok(WorkResultStatus::Blocked),
        "cancelled" => Ok(WorkResultStatus::Cancelled),
        other => Err(StorageError::Internal(format!(
            "invalid work result status {other}"
        ))),
    }
}

fn acceptance_verification_status_to_str(value: AcceptanceVerificationStatus) -> &'static str {
    value.as_str()
}

fn parse_acceptance_verification_status(
    value: &str,
) -> Result<AcceptanceVerificationStatus, StorageError> {
    match value {
        "passed" => Ok(AcceptanceVerificationStatus::Passed),
        "failed" => Ok(AcceptanceVerificationStatus::Failed),
        "skipped" => Ok(AcceptanceVerificationStatus::Skipped),
        "blocked" => Ok(AcceptanceVerificationStatus::Blocked),
        other => Err(StorageError::Internal(format!(
            "invalid acceptance verification status {other}"
        ))),
    }
}

proxima_storage_pg::pg_sidecar! {
    payload: CommitV1,
    row: CommitPayloadRow,
    kinds: [Fact],
    table: "proxima_code.commit_v1",
    key: t,
    fields: {
        repo_id => repo_id: (uuid),
        sha => sha: (text),
        parents => parents: (text_array),
        author_name => author_name: (text),
        author_email => author_email: (text),
        author_time => author_time: (timestamptz),
        committer_name => committer_name: (text),
        committer_email => committer_email: (text),
        committer_time => committer_time: (timestamptz),
        message => message: (text),
    },
}

proxima_storage_pg::pg_sidecar! {
    payload: FileRevisionV1,
    row: FileRevisionPayloadRow,
    kinds: [Fact],
    table: "proxima_code.file_revision_v1",
    key: t,
    fields: {
        repo_id => repo_id: (uuid),
        file_path => file_path: (text),
        language => language: (opt_text),
        content_sha256 => content_sha256: (bytea32),
        size_bytes => size_bytes: (u64_as_i64_saturating),
        indexed_commit_sha => indexed_commit_sha: (text),
        state => state: (enum_copy {
            to_str: file_state_to_str,
            pg_type: "proxima_code.file_state",
            from_str: parse_file_state
        }),
    },
}

proxima_storage_pg::pg_sidecar! {
    payload: ExecutionRequestV1,
    row: ExecutionRequestPayloadRow,
    kinds: [Fact],
    table: "proxima_code.work_requested_v1",
    key: t,
    fields: {
        repo_id => repo_id: (uuid),
        title => title: (text),
        instructions => instructions: (text),
        request_key => request_key: (text),
        depends_on_memory_ids => depends_on_memory_ids: (uuid_array),
    },
}

proxima_storage_pg::pg_sidecar! {
    payload: ExecutionResultV1,
    row: ExecutionResultPayloadRow,
    kinds: [Fact],
    table: "proxima_code.execution_result_v1",
    key: t,
    fields: {
        work_requested_memory_id => work_requested_memory_id: (uuid),
        repo_id => repo_id: (uuid),
        status => status: (enum_copy {
            to_str: work_result_status_to_str,
            pg_type: "proxima_code.work_result_status",
            from_str: parse_work_result_status
        }),
        summary => summary: (text),
        artifact_refs => artifact_refs: (text_array),
        log_excerpt => log_excerpt: (opt_text),
    },
}

proxima_storage_pg::pg_sidecar! {
    payload: TestResultV1,
    row: TestResultPayloadRow,
    kinds: [Fact],
    table: "proxima_code.test_result_v1",
    key: t,
    fields: {
        test_requested_memory_id => test_requested_memory_id: (uuid),
        repo_id => repo_id: (uuid),
        status => status: (enum_copy {
            to_str: work_result_status_to_str,
            pg_type: "proxima_code.work_result_status",
            from_str: parse_work_result_status
        }),
        summary => summary: (text),
        artifact_refs => artifact_refs: (text_array),
        log_excerpt => log_excerpt: (opt_text),
    },
}

proxima_storage_pg::pg_sidecar! {
    payload: AcceptanceVerificationV1,
    row: AcceptanceVerificationPayloadRow,
    kinds: [Fact],
    table: "proxima_code.acceptance_verification_v1",
    key: t,
    fields: {
        work_item_memory_id => work_item_memory_id: (uuid),
        criterion_key => criterion_key: (text),
        status => status: (enum_copy {
            to_str: acceptance_verification_status_to_str,
            pg_type: "proxima_code.acceptance_verification_status",
            from_str: parse_acceptance_verification_status
        }),
        summary => summary: (text),
        artifact_refs => artifact_refs: (text_array),
        verifier_memory_id => verifier_memory_id: (opt_uuid),
    },
}

// Hand-written rather than `pg_sidecar!`: a chunk carries a nested call
// list, and the macro maps one payload field to one column. The sites live
// in a child table keyed by (chunk, callee), which is what makes "ten call
// sites, one index row" storable — the index cannot hold ten rows for one
// pair, and the payload must.
impl PgMemorySidecar for CodeChunkV1 {
    fn insert_memory_sidecar<'t>(
        &'t self,
        tx: &'t mut Transaction<'_, Postgres>,
        memory_id: MemoryId,
        _permit: SidecarInsertPermit,
    ) -> PgSidecarFuture<'t> {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO proxima_code.code_chunk_v1
                    (t, repo_id, file_path, chunk_index, text, language, chunk_type,
                     byte_range_start, byte_range_end, line_range_start, line_range_end, state)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11,
                         $12::proxima_code.file_state)",
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
            .bind(file_state_to_str(self.state))
            .execute(tx.as_mut())
            .await
            .map_err(proxima_storage_pg::map_err)?;
            for call in &self.calls {
                for (index, site) in call.sites.iter().enumerate() {
                    sqlx::query(
                        "INSERT INTO proxima_code.code_chunk_call_v1
                            (caller_memory_id, callee_memory_id, site_index,
                             byte_start, byte_end, callee_name, is_dynamic)
                         VALUES ($1, $2, $3, $4, $5, $6, $7)",
                    )
                    .bind(memory_id.into_inner())
                    .bind(call.callee_memory_id)
                    .bind(i32::try_from(index).unwrap_or(i32::MAX))
                    .bind(i64::from(site.byte_start))
                    .bind(i64::from(site.byte_end))
                    .bind(&site.callee_name)
                    .bind(site.is_dynamic)
                    .execute(tx.as_mut())
                    .await
                    .map_err(proxima_storage_pg::map_err)?;
                }
            }
            Ok(())
        })
    }
}

#[derive(Debug, sqlx::FromRow)]
struct CodeChunkPayloadRow {
    t: uuid::Uuid,
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
    state: String,
}

#[derive(Debug, sqlx::FromRow)]
struct CodeChunkCallRow {
    caller_memory_id: uuid::Uuid,
    callee_memory_id: uuid::Uuid,
    byte_start: i64,
    byte_end: i64,
    callee_name: String,
    is_dynamic: bool,
}

impl PgMemoryPayload for CodeChunkV1 {
    fn load_batch<'t>(
        ctx: PgSidecarReadCtx<'t>,
        kind: PayloadKind,
        memory_ids: &'t [MemoryId],
    ) -> PgMemoryPayloadBatchFuture<'t> {
        Box::pin(async move {
            if memory_ids.is_empty() || kind != PayloadKind::Abstraction {
                return Ok(Vec::new());
            }
            let rows: Vec<CodeChunkPayloadRow> = ctx
                .fetch_all_by_memory_ids(
                    "SELECT t, repo_id, file_path, chunk_index, text, language,
                            chunk_type, byte_range_start, byte_range_end,
                            line_range_start, line_range_end, state::text AS state
                       FROM proxima_code.code_chunk_v1
                      WHERE t = ANY($1::uuid[])",
                    memory_ids,
                )
                .await?;
            // One extra query for the whole page, not one per chunk: the
            // call list is part of the payload, so it must not turn a
            // search page into N round trips.
            let call_rows: Vec<CodeChunkCallRow> = ctx
                .fetch_all_by_memory_ids(
                    "SELECT caller_memory_id, callee_memory_id, byte_start, byte_end,
                            callee_name, is_dynamic
                       FROM proxima_code.code_chunk_call_v1
                      WHERE caller_memory_id = ANY($1::uuid[])
                      ORDER BY caller_memory_id, callee_memory_id, site_index",
                    memory_ids,
                )
                .await?;
            let mut calls_by_caller: BTreeMap<uuid::Uuid, Vec<CodeCallV1>> = BTreeMap::new();
            for row in call_rows {
                let entry = calls_by_caller.entry(row.caller_memory_id).or_default();
                let site = CodeCallSiteV1 {
                    byte_start: u32::try_from(row.byte_start).unwrap_or(u32::MAX),
                    byte_end: u32::try_from(row.byte_end).unwrap_or(u32::MAX),
                    callee_name: row.callee_name,
                    is_dynamic: row.is_dynamic,
                };
                match entry
                    .last_mut()
                    .filter(|call| call.callee_memory_id == row.callee_memory_id)
                {
                    Some(call) => call.sites.push(site),
                    None => entry.push(CodeCallV1 {
                        callee_memory_id: row.callee_memory_id,
                        sites: vec![site],
                    }),
                }
            }
            rows.into_iter()
                .map(|row| {
                    let memory_id = MemoryId::new(row.t);
                    let payload = CodeChunkV1 {
                        repo_id: row.repo_id,
                        file_path: row.file_path,
                        chunk_index: u32::try_from(row.chunk_index).unwrap_or(u32::MAX),
                        text: row.text,
                        language: row.language,
                        chunk_type: row.chunk_type,
                        byte_range_start: u32::try_from(row.byte_range_start).unwrap_or(u32::MAX),
                        byte_range_end: u32::try_from(row.byte_range_end).unwrap_or(u32::MAX),
                        line_range_start: u32::try_from(row.line_range_start).unwrap_or(u32::MAX),
                        line_range_end: u32::try_from(row.line_range_end).unwrap_or(u32::MAX),
                        state: parse_file_state(&row.state)?,
                        calls: calls_by_caller.get(&row.t).cloned().unwrap_or_default(),
                    };
                    Ok((memory_id, SidecarPayload::abstraction(payload)))
                })
                .collect::<Result<Vec<_>, StorageError>>()
        })
    }
}

proxima_storage_pg::pg_sidecar! {
    payload: CommitSummaryV1,
    row: CommitSummaryPayloadRow,
    kinds: [Abstraction],
    table: "proxima_code.commit_summary_v1",
    key: t,
    fields: {
        repo_id => repo_id: (uuid),
        commit_sha => commit_sha: (text),
        summary => summary: (text),
        key_files => key_files: (text_array),
        change_kind => change_kind: (text),
    },
}

proxima_storage_pg::pg_sidecar! {
    payload: AcceptanceSummaryV1,
    row: AcceptanceSummaryPayloadRow,
    kinds: [Abstraction],
    table: "proxima_code.acceptance_summary_v1",
    key: t,
    fields: {
        work_item_memory_id => work_item_memory_id: (uuid),
        repo_id => repo_id: (uuid),
        passed_required => passed_required: (bool),
        summary => summary: (text),
        verification_memory_ids => verification_memory_ids: (uuid_array),
    },
}

proxima_storage_pg::pg_sidecar! {
    payload: CodeDevelopmentPerspectiveV1,
    row: CodeDevelopmentPerspectivePayloadRow,
    kinds: [Perspective],
    table: "proxima_code.development_perspective_v1",
    key: t,
    fields: {
        repo_id => repo_id: (opt_uuid),
        summary => summary: (text),
        pattern => pattern: (text),
        risk => risk: (text),
        recommended_posture => recommended_posture: (text),
        confidence => confidence: (f32),
    },
}

proxima_storage_pg::pg_sidecar! {
    payload: CodeCommitSummarizerSelfV1,
    row: CodeCommitSummarizerSelfPayloadRow,
    kinds: [Perspective],
    table: "proxima_code.commit_summarizer_self_v1",
    key: t,
    fields: {
        display_name => display_name: (text),
        purpose => purpose: (text),
    },
}

proxima_storage_pg::pg_sidecar! {
    payload: CodeEngineerSelfV1,
    row: CodeEngineerSelfPayloadRow,
    kinds: [Perspective],
    table: "proxima_code.engineer_self_v1",
    key: t,
    fields: {
        display_name => display_name: (text),
        purpose => purpose: (text),
    },
}

proxima_storage_pg::pg_sidecar! {
    payload: CodeWorkAssignmentV1,
    row: CodeWorkAssignmentPayloadRow,
    kinds: [Perspective],
    table: "proxima_code.work_assignment_v1",
    key: t,
    fields: {
        repo_id => repo_id: (uuid),
        target_perspective_memory_id => target_perspective_memory_id: (uuid),
        work_item_memory_id => work_item_memory_id: (uuid),
        reason => reason: (text),
    },
}

impl PgFactSidecar for AcceptanceCriteriaV1 {
    fn insert_sidecar<'t>(
        self,
        tx: &'t mut Transaction<'_, Postgres>,
        memory_id: MemoryId,
        _permit: SidecarInsertPermit,
    ) -> FactIngestSidecarFuture<'t>
    where
        Self: 't,
    {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO proxima_code.acceptance_criteria_v1
                    (t, work_item_memory_id, criteria_count)
                 VALUES ($1, $2, $3)",
            )
            .bind(memory_id.into_inner())
            .bind(self.work_item_memory_id)
            .bind(i32::try_from(self.criteria.len()).unwrap_or(i32::MAX))
            .execute(tx.as_mut())
            .await
            .map_err(proxima_storage_pg::map_err)?;
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
    // N+1 per work item; acceptable at this cardinality.
    fn load_memory_payload(
        ctx: PgSidecarReadCtx<'_>,
        memory_id: MemoryId,
    ) -> PgMemoryPayloadFuture<'_> {
        Box::pin(async move {
            let work_item_memory_id: Option<uuid::Uuid> = ctx
                .fetch_optional_scalar_by_memory_id(
                    "SELECT work_item_memory_id
                       FROM proxima_code.acceptance_criteria_v1
                      WHERE t = $1",
                    memory_id,
                )
                .await?;
            let Some(work_item_memory_id) = work_item_memory_id else {
                return Ok(None);
            };
            let criteria = load_criteria_rows(
                ctx,
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
        _permit: SidecarInsertPermit,
    ) -> FactIngestSidecarFuture<'t>
    where
        Self: 't,
    {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO proxima_code.test_requested_v1
                    (t, repo_id, title, instructions, test_key, criteria_count,
                     depends_on_memory_ids)
                 VALUES ($1, $2, $3, $4, $5, $6, $7)",
            )
            .bind(memory_id.into_inner())
            .bind(self.repo_id)
            .bind(&self.title)
            .bind(&self.instructions)
            .bind(&self.test_key)
            .bind(i32::try_from(self.criteria.len()).unwrap_or(i32::MAX))
            .bind(&self.depends_on_memory_ids)
            .execute(tx.as_mut())
            .await
            .map_err(proxima_storage_pg::map_err)?;
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
    // N+1 per work item; acceptable at this cardinality.
    fn load_memory_payload(
        ctx: PgSidecarReadCtx<'_>,
        memory_id: MemoryId,
    ) -> PgMemoryPayloadFuture<'_> {
        Box::pin(async move {
            let row: Option<(uuid::Uuid, String, String, String, Vec<uuid::Uuid>)> = ctx
                .fetch_optional_by_memory_id(
                    "SELECT repo_id, title, instructions, test_key, depends_on_memory_ids
                       FROM proxima_code.test_requested_v1
                      WHERE t = $1",
                    memory_id,
                )
                .await?;
            let Some((repo_id, title, instructions, test_key, depends_on_memory_ids)) = row else {
                return Ok(None);
            };
            let criteria = load_criteria_rows(
                ctx,
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
                depends_on_memory_ids,
            })))
        })
    }
}

impl PgMemorySidecar for CodeExecutionPlanV1 {
    fn insert_memory_sidecar<'t>(
        &'t self,
        tx: &'t mut Transaction<'_, Postgres>,
        memory_id: MemoryId,
        _permit: SidecarInsertPermit,
    ) -> PgSidecarFuture<'t> {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO proxima_code.execution_plan_v1
                    (t, repo_id, plan_key, goal_activated_memory_id,
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
            .map_err(proxima_storage_pg::map_err)?;
            for (index, item) in self.items.iter().enumerate() {
                sqlx::query(
                    "INSERT INTO proxima_code.execution_plan_item_v1
                        (plan_memory_id, item_index, item_key, kind,
                         title, depends_on, request_key, request_memory_id)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
                )
                .bind(memory_id.into_inner())
                .bind(i32::try_from(index).unwrap_or(i32::MAX))
                .bind(&item.key)
                .bind(item.kind)
                .bind(&item.title)
                .bind(&item.depends_on)
                .bind(&item.request_key)
                .bind(item.request_memory_id)
                .execute(tx.as_mut())
                .await
                .map_err(proxima_storage_pg::map_err)?;
            }
            Ok(())
        })
    }
}

#[derive(Debug, sqlx::FromRow)]
struct ExecutionPlanItemPayloadRow {
    item_key: String,
    request_memory_id: uuid::Uuid,
    kind: CodeExecutionPlanItemKind,
    title: String,
    depends_on: Vec<String>,
    request_key: String,
}

impl PgMemoryPayload for CodeExecutionPlanV1 {
    // N+1 per work item; acceptable at this cardinality.
    fn load_memory_payload(
        ctx: PgSidecarReadCtx<'_>,
        memory_id: MemoryId,
    ) -> PgMemoryPayloadFuture<'_> {
        Box::pin(async move {
            let row: Option<(uuid::Uuid, String, uuid::Uuid, String, Vec<uuid::Uuid>)> = ctx
                .fetch_optional_by_memory_id(
                    "SELECT repo_id, plan_key, goal_activated_memory_id,
                            summary, evidence_memory_ids
                       FROM proxima_code.execution_plan_v1
                      WHERE t = $1",
                    memory_id,
                )
                .await?;
            let Some((repo_id, plan_key, goal_activated_memory_id, summary, evidence_memory_ids)) =
                row
            else {
                return Ok(None);
            };
            let item_rows: Vec<ExecutionPlanItemPayloadRow> = ctx
                .fetch_all_by_memory_id(
                    "SELECT item_key, kind, title, depends_on, request_key, request_memory_id
                       FROM proxima_code.execution_plan_item_v1
                      WHERE plan_memory_id = $1
                      ORDER BY item_index ASC",
                    memory_id,
                )
                .await?;
            let items = item_rows
                .into_iter()
                .map(|row| CodeExecutionPlanItemV1 {
                    key: row.item_key,
                    kind: row.kind,
                    title: row.title,
                    depends_on: row.depends_on,
                    request_key: row.request_key,
                    request_memory_id: row.request_memory_id,
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
