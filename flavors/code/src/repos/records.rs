use std::str::FromStr;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct RepoRecord {
    pub repo_id: Uuid,
    pub canonical_path: String,
    pub display_name: String,
    pub target_branch: Option<String>,
    pub last_cursor: Option<Vec<u8>>,
    pub last_polled_at: Option<time::OffsetDateTime>,
    pub created_at: time::OffsetDateTime,
    /// Which of the repository's paths ingest indexes. Empty lists mean
    /// every tracked blob under the size cap, which is what every repo
    /// registered before scoping existed has.
    pub scope: super::scope::RepoScope,
}

#[derive(Debug, Clone)]
pub struct RepoEraseReceipt {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(
    type_name = "proxima_code.repo_ingestion_run_status",
    rename_all = "snake_case"
)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(
    type_name = "proxima_code.repo_ingestion_run_stage",
    rename_all = "snake_case"
)]
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

#[derive(Debug, Clone, serde::Serialize)]
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
    #[error("storage error: {0}")]
    Storage(#[from] proxima_core::StorageError),
    #[error("duplicate repo at path: {canonical_path}")]
    DuplicatePath { canonical_path: String },
    #[error("repo not found: {repo_id}")]
    NotFound { repo_id: Uuid },
    #[error("invalid target branch for repo {repo_id}: {target_branch} ({reason})")]
    InvalidTargetBranch {
        repo_id: Uuid,
        target_branch: String,
        reason: String,
    },
    #[error("invalid ingest scope for repo {repo_id}: {source}")]
    InvalidScope {
        repo_id: Uuid,
        #[source]
        source: super::scope::ScopeError,
    },
    #[error("ingestion run not found: {run_id}")]
    RunNotFound { run_id: Uuid },
    #[error("ingestion run is already in terminal state: {run_id} ({status:?})")]
    RunAlreadyTerminal { run_id: Uuid, status: RunStatus },
}
