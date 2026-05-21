// ---------------------------------------------------------------
// Repo registry commands (M6.S2) — DB-backed repo registry for
// LocalGitSource ingestion. Each handler pulls Arc<PgStorage> from
// Tauri state and uses the sentinel owner.
// ---------------------------------------------------------------

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct RepoRecordTs {
    pub repo_id: String,
    pub canonical_path: String,
    pub display_name: String,
    pub target_branch: Option<String>,
    pub has_been_polled: bool,
    pub last_polled_at: Option<String>,
    pub created_at: String,
}

impl From<proxima_code::RepoRecord> for RepoRecordTs {
    fn from(r: proxima_code::RepoRecord) -> Self {
        use time::format_description::well_known::Rfc3339;
        Self {
            repo_id: r.repo_id.to_string(),
            canonical_path: r.canonical_path,
            display_name: r.display_name,
            target_branch: r.target_branch,
            has_been_polled: r.last_polled_at.is_some(),
            last_polled_at: r.last_polled_at.map(|t| {
                t.format(&Rfc3339)
                    .expect("OffsetDateTime always formats as RFC3339")
            }),
            created_at: r
                .created_at
                .format(&Rfc3339)
                .expect("OffsetDateTime always formats as RFC3339"),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, specta::Type)]
pub struct IngestProgressTs {
    pub commit_index: u32,
    pub total_commits: u32,
    pub commit_sha: String,
    pub commits_emitted: u32,
    pub commits_replayed: u32,
    pub chunks_emitted: u32,
    pub chunks_reused: u32,
}

impl From<proxima_code::IngestProgress> for IngestProgressTs {
    fn from(p: proxima_code::IngestProgress) -> Self {
        Self {
            commit_index: u32::try_from(p.commit_index).unwrap_or(u32::MAX),
            total_commits: u32::try_from(p.total_commits).unwrap_or(u32::MAX),
            commit_sha: p.commit_sha,
            commits_emitted: u32::try_from(p.commits_emitted).unwrap_or(u32::MAX),
            commits_replayed: u32::try_from(p.commits_replayed).unwrap_or(u32::MAX),
            chunks_emitted: u32::try_from(p.chunks_emitted).unwrap_or(u32::MAX),
            chunks_reused: u32::try_from(p.chunks_reused).unwrap_or(u32::MAX),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, specta::Type)]
pub struct IndexReportTs {
    pub commits_emitted: u32,
    pub commits_replayed: u32,
    pub files_present_emitted: u32,
    pub files_tombstoned: u32,
    pub chunks_emitted: u32,
    pub chunks_reused: u32,
    pub chunks_tombstoned: u32,
}

impl From<proxima_code::IndexReport> for IndexReportTs {
    fn from(r: proxima_code::IndexReport) -> Self {
        Self {
            commits_emitted: u32::try_from(r.commits_emitted).unwrap_or(u32::MAX),
            commits_replayed: u32::try_from(r.commits_replayed).unwrap_or(u32::MAX),
            files_present_emitted: u32::try_from(r.files_present_emitted).unwrap_or(u32::MAX),
            files_tombstoned: u32::try_from(r.files_tombstoned).unwrap_or(u32::MAX),
            chunks_emitted: u32::try_from(r.chunks_emitted).unwrap_or(u32::MAX),
            chunks_reused: u32::try_from(r.chunks_reused).unwrap_or(u32::MAX),
            chunks_tombstoned: u32::try_from(r.chunks_tombstoned).unwrap_or(u32::MAX),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, specta::Type)]
#[serde(tag = "kind", content = "data", rename_all = "camelCase")]
pub enum RepoIngestEventTs {
    Progress(IngestProgressTs),
    Snapshot(RepoIngestionRunTs),
    Done(IndexReportTs),
    Error { message: String },
}

/// Anchors flavor payload types in generated TypeScript bindings.
/// The command is never used by the UI; Specta exports types through
/// command signatures, so optional fields keep this cheap if invoked.
#[derive(Debug, Default, serde::Serialize, specta::Type)]
pub struct PayloadTypesAnchor {
    pub file_revision_v1: Option<proxima_code::FileRevisionV1>,
    pub code_chunk_v1: Option<proxima_code::CodeChunkV1>,
    pub workspace_decision: Option<proxima_code::WorkspaceDecision>,
    pub workspace_review_verdict: Option<proxima_code::WorkspaceReviewVerdict>,
}

#[tauri::command]
#[specta::specta]
pub fn payload_types_anchor() -> PayloadTypesAnchor {
    PayloadTypesAnchor::default()
}

#[derive(Debug, Clone, serde::Serialize, specta::Type)]
pub struct RepoEraseReceiptTs {
    pub repo_id: String,
    pub completed_at: String,
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

impl From<proxima_code::RepoEraseReceipt> for RepoEraseReceiptTs {
    fn from(r: proxima_code::RepoEraseReceipt) -> Self {
        use time::format_description::well_known::Rfc3339;
        Self {
            repo_id: r.repo_id.to_string(),
            completed_at: r
                .completed_at
                .format(&Rfc3339)
                .expect("OffsetDateTime always formats as RFC3339"),
            facts_deleted: r.facts_deleted,
            abstractions_deleted: r.abstractions_deleted,
            edges_deleted: r.edges_deleted,
            embeddings_deleted: r.embeddings_deleted,
            events_deleted: r.events_deleted,
            citation_mappings_deleted: r.citation_mappings_deleted,
            cited_objects_deleted: r.cited_objects_deleted,
            source_batches_deleted: r.source_batches_deleted,
            f2a_rows_deleted: r.f2a_rows_deleted,
            repo_record_deleted: r.repo_record_deleted,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, specta::Type)]
pub struct RepoIngestionRunTs {
    pub run_id: String,
    pub repo_id: String,
    pub status: proxima_code::RunStatus,
    pub stage: proxima_code::RunStage,
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
    pub started_at: String,
    pub updated_at: String,
    pub finished_at: Option<String>,
}

impl From<proxima_code::RepoIngestionRun> for RepoIngestionRunTs {
    fn from(r: proxima_code::RepoIngestionRun) -> Self {
        use time::format_description::well_known::Rfc3339;
        Self {
            run_id: r.run_id.to_string(),
            repo_id: r.repo_id.to_string(),
            status: r.status,
            stage: r.stage,
            commits_emitted: r.commits_emitted,
            files_emitted: r.files_emitted,
            chunks_emitted: r.chunks_emitted,
            chunks_reused: r.chunks_reused,
            chunks_tombstoned: r.chunks_tombstoned,
            ast_edges_emitted: r.ast_edges_emitted,
            abstractions_emitted: r.abstractions_emitted,
            embeddings_landed: r.embeddings_landed,
            citations_emitted: r.citations_emitted,
            error_message: r.error_message,
            started_at: r
                .started_at
                .format(&Rfc3339)
                .expect("OffsetDateTime always formats as RFC3339"),
            updated_at: r
                .updated_at
                .format(&Rfc3339)
                .expect("OffsetDateTime always formats as RFC3339"),
            finished_at: r.finished_at.map(|t| {
                t.format(&Rfc3339)
                    .expect("OffsetDateTime always formats as RFC3339")
            }),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, specta::Type)]
pub struct WorkspaceReviewRecordTs {
    pub memory_id: String,
    pub workspace_run_memory_id: String,
    pub execution_request_memory_id: String,
    pub verdict: proxima_code::WorkspaceReviewVerdict,
    pub round_index: u32,
    pub summary: String,
    pub findings: Vec<proxima_code::WorkspaceReviewFinding>,
    pub correction_instructions: Option<String>,
    pub verification_summary: Option<String>,
    pub reviewed_at: String,
    pub created_at: String,
}

impl From<proxima_code::WorkspaceReviewRecord> for WorkspaceReviewRecordTs {
    fn from(r: proxima_code::WorkspaceReviewRecord) -> Self {
        use time::format_description::well_known::Rfc3339;
        Self {
            memory_id: r.memory_id.to_string(),
            workspace_run_memory_id: r.workspace_run_memory_id.to_string(),
            execution_request_memory_id: r.execution_request_memory_id.to_string(),
            verdict: r.verdict,
            round_index: r.round_index,
            summary: r.summary,
            findings: r.findings,
            correction_instructions: r.correction_instructions,
            verification_summary: r.verification_summary,
            reviewed_at: r
                .reviewed_at
                .format(&Rfc3339)
                .expect("OffsetDateTime always formats as RFC3339"),
            created_at: r
                .created_at
                .format(&Rfc3339)
                .expect("OffsetDateTime always formats as RFC3339"),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, specta::Type)]
pub struct WorkspaceDecisionRecordTs {
    pub memory_id: String,
    pub workspace_run_memory_id: String,
    pub decision: proxima_code::WorkspaceDecision,
    pub decided_at: String,
    pub reason_text: Option<String>,
    pub decided_by_owner_id: String,
}

impl From<proxima_code::WorkspaceDecisionRecord> for WorkspaceDecisionRecordTs {
    fn from(r: proxima_code::WorkspaceDecisionRecord) -> Self {
        use time::format_description::well_known::Rfc3339;
        Self {
            memory_id: r.memory_id.to_string(),
            workspace_run_memory_id: r.workspace_run_memory_id.to_string(),
            decision: r.decision,
            decided_at: r
                .decided_at
                .format(&Rfc3339)
                .expect("OffsetDateTime always formats as RFC3339"),
            reason_text: r.reason_text,
            decided_by_owner_id: r.decided_by_owner_id.to_string(),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, specta::Type)]
pub struct WorkspaceRunRecordTs {
    pub memory_id: String,
    pub wake_invocation_id: String,
    pub repo_id: String,
    pub execution_request_title: Option<String>,
    pub target_branch: String,
    pub worktree_path: String,
    pub branch_name: String,
    pub parent_sha: String,
    pub head_sha: String,
    pub diff_stat_json: proxima_core::CoreWorkspaceDiffStat,
    pub exit_code: Option<i32>,
    pub stdout_tail: Option<String>,
    pub stderr_tail: Option<String>,
    pub duration_ms: Option<u64>,
    pub created_at: String,
    pub latest_review: Option<WorkspaceReviewRecordTs>,
    pub latest_decision: Option<WorkspaceDecisionRecordTs>,
}

impl From<proxima_code::WorkspaceRunRecord> for WorkspaceRunRecordTs {
    fn from(r: proxima_code::WorkspaceRunRecord) -> Self {
        use time::format_description::well_known::Rfc3339;
        Self {
            memory_id: r.memory_id.to_string(),
            wake_invocation_id: r.wake_invocation_id.to_string(),
            repo_id: r.repo_id.to_string(),
            execution_request_title: r.execution_request_title,
            target_branch: r.target_branch,
            worktree_path: r.worktree_path,
            branch_name: r.branch_name,
            parent_sha: r.parent_sha,
            head_sha: r.head_sha,
            diff_stat_json: r.diff_stat_json,
            exit_code: r.exit_code,
            stdout_tail: r.stdout_tail,
            stderr_tail: r.stderr_tail,
            duration_ms: r.duration_ms,
            created_at: r
                .created_at
                .format(&Rfc3339)
                .expect("OffsetDateTime always formats as RFC3339"),
            latest_review: r.latest_review.map(Into::into),
            latest_decision: r.latest_decision.map(Into::into),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, specta::Type)]
pub struct WorkspaceRunDiffTs {
    pub range: String,
    pub stat: String,
    pub files: Vec<String>,
    pub patch: String,
    pub patch_truncated: bool,
    pub max_patch_bytes: usize,
}

impl From<proxima_code::WorkspaceRunDiff> for WorkspaceRunDiffTs {
    fn from(r: proxima_code::WorkspaceRunDiff) -> Self {
        Self {
            range: r.range,
            stat: r.stat,
            files: r.files,
            patch: r.patch,
            patch_truncated: r.patch_truncated,
            max_patch_bytes: r.max_patch_bytes,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, specta::Type)]
pub struct WorkspaceMergeOutcomeTs {
    pub run_memory_id: String,
    pub decision_memory_id: String,
    pub repo_id: String,
    pub target_branch: String,
    pub old_target_sha: String,
    pub new_target_sha: String,
}

impl From<proxima_code::WorkspaceMergeOutcome> for WorkspaceMergeOutcomeTs {
    fn from(r: proxima_code::WorkspaceMergeOutcome) -> Self {
        Self {
            run_memory_id: r.run_memory_id.to_string(),
            decision_memory_id: r.decision_memory_id.to_string(),
            repo_id: r.repo_id.to_string(),
            target_branch: r.target_branch,
            old_target_sha: r.old_target_sha,
            new_target_sha: r.new_target_sha,
        }
    }
}
