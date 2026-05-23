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

