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

/// What `proxima-code_erase_repo` actually did.
///
/// Five of the seven counters this replaced were structurally zero:
/// `abstractions_deleted`, `edges_deleted`, `embeddings_deleted`,
/// `receipts_deleted` and `source_batches_deleted` were literal `0`s in the
/// only code that built the receipt, and `facts_deleted` counted every
/// admission the erase touched — abstractions included — so its name was
/// wrong too. A caller reading `embeddings_deleted: 0` after an erase that
/// deleted embeddings was being told something false.
///
/// The kernel does not report per-kind counts because it does not delete by
/// kind: it deletes admissions, and each admission takes its sidecar,
/// embedding, sketch and projection rows with it. Reporting the number it
/// actually has is the honest surface.
#[derive(Debug, Clone)]
pub struct RepoEraseReceipt {
    pub repo_id: Uuid,
    pub completed_at: time::OffsetDateTime,
    /// Admissions erased: every version of every series this repo's rows
    /// named, Facts and Abstractions and Perspectives alike.
    pub memories_deleted: u64,
    /// Cold objects marked for destruction. They are destroyed by
    /// `maintain-retention --retry-cold-object-purges`, not by the erase —
    /// see `super::erase::erase_repo`.
    pub cold_objects_pending: u64,
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
    /// Another principal's rows point at memories filed under this repo.
    ///
    /// A code sidecar's foreign key names `proxima_core.memory (t)` and
    /// nothing in it constrains whose memory that is, so one owner's row can
    /// reference another's admission — most ordinarily because the
    /// admission was transferred. The repo erase deletes a reference to
    /// erased data along with what it points at, which is settled policy
    /// WITHIN one owner and no one's to decide ACROSS two: erasing here
    /// would destroy a principal's rows on a different principal's say-so,
    /// and silently, since the referencing memory itself would survive with
    /// its sidecar gone. So the erase stops and names the rows instead.
    #[error(
        "repo {repo_id} is referenced by rows another principal owns; erasing them is not \
         this owner's to do — resolve or transfer them first: {}",
        blocking.join(", ")
    )]
    CrossOwnerReference {
        repo_id: Uuid,
        blocking: Vec<String>,
    },
    /// The sweep deleted a row the footprint never named.
    ///
    /// Two causes, and the common one is not a bug. An ordinary write
    /// committed between the discovery pass and the lock is a row the
    /// footprint could not have seen and the sweep then reaches — which is
    /// the guard doing exactly its job, and which re-discovery fixes, so
    /// the erase treats it as transient and comes round again. The other
    /// cause is that the finding statements and the deleting statements
    /// have drifted apart, which no retry fixes and which surfaces here
    /// once the budget is spent.
    ///
    /// Either way the answer is to refuse: the footprint is what the erase
    /// locks, so a row outside it is a row deleted under no lock. The
    /// alternative is an erase that looks like it worked.
    #[error(
        "repo {repo_id} erase reached memory {memory_id}, which its footprint never named — \
         a concurrent write landed in the discovery window, or the finder and the sweep \
         have drifted apart"
    )]
    FootprintIncomplete { repo_id: Uuid, memory_id: Uuid },
    #[error("ingestion run not found: {run_id}")]
    RunNotFound { run_id: Uuid },
    #[error("ingestion run is already in terminal state: {run_id} ({status:?})")]
    RunAlreadyTerminal { run_id: Uuid, status: RunStatus },
}
