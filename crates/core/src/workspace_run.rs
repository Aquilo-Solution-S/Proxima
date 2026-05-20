//! Core workspace-run Fact payload.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::verbs::event_ingest::{CitationMappingHint, CitedObjectHint, EventDraft};
use crate::{
    FactPayload, MemoryId, Owner, SchemaId, SchemaVersion, SourceBatchId, SourceId,
    proxima_schema_id,
};

pub const CORE_WORKSPACE_RUN_SOURCE_ID: &str = "proxima-core/workspace-runner";
pub const CORE_WORKSPACE_RUN_OBJECT_SCHEMA: &str = proxima_schema_id!("workspace-run-object-v1");
pub const CORE_WORKSPACE_RUN_WHOLE_SCHEMA: &str = proxima_schema_id!("workspace-run-whole-v1");

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
pub struct CoreWorkspaceDiffFile {
    pub path: String,
    pub insertions: u64,
    pub deletions: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
pub struct CoreWorkspaceDiffStat {
    pub files_changed: u64,
    pub insertions: u64,
    pub deletions: u64,
    pub files: Vec<CoreWorkspaceDiffFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
pub struct CoreWorkspaceRunV1 {
    pub wake_invocation_id: Uuid,
    pub wake_entry_id: Uuid,
    pub personality_instance_id: Uuid,
    pub binding_kind: String,
    pub finalize: String,
    pub repo_path: String,
    pub base_ref: String,
    pub worktree_path: String,
    pub branch_name: String,
    pub parent_sha: String,
    pub head_sha: String,
    pub committed: bool,
    pub diff_stat_json: CoreWorkspaceDiffStat,
    pub exit_code: Option<i32>,
    pub stdout_tail: Option<String>,
    pub stderr_tail: Option<String>,
    pub duration_ms: Option<u64>,
}

impl FactPayload for CoreWorkspaceRunV1 {
    const SCHEMA_ID: &'static str = proxima_schema_id!("workspace-run-v1");
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_core.workspace_run_v1"
    }

    fn render(&self) -> String {
        let short_head = self.head_sha.get(..7).unwrap_or(&self.head_sha);
        format!("Core workspace run {} at {short_head}", self.branch_name)
    }
}

#[derive(Debug, Clone)]
pub struct CoreWorkspaceRunPersistInput {
    pub owner: Owner,
    pub root_perspective_memory_id: MemoryId,
    pub triggering_memory_id: MemoryId,
    pub run: CoreWorkspaceRunV1,
    pub source_batch_id: SourceBatchId,
    pub source_id: SourceId,
    pub observed_at: time::OffsetDateTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoreWorkspaceRunPersistOutcome {
    pub memory_id: MemoryId,
    pub change_event_seq: Uuid,
    pub idempotent_replay: bool,
}

pub fn core_workspace_run_event_draft(
    owner: Owner,
    payload: &[u8],
    source_batch_id: SourceBatchId,
    source_id: SourceId,
    observed_at: time::OffsetDateTime,
) -> EventDraft {
    let content_hash = blake3::hash(payload);
    EventDraft {
        source_id,
        source_batch_id,
        owner,
        schema_id: SchemaId::new(CoreWorkspaceRunV1::SCHEMA_ID.into()),
        schema_version: SchemaVersion::new(CoreWorkspaceRunV1::SCHEMA_VERSION),
        payload: payload.to_vec(),
        observed_at,
        occurred_at: observed_at,
        cited_object: CitedObjectHint {
            schema_id: SchemaId::new(CORE_WORKSPACE_RUN_OBJECT_SCHEMA.into()),
            schema_version: SchemaVersion::new(1),
            content_hash: *content_hash.as_bytes(),
        },
        citation_mapping: CitationMappingHint {
            schema_id: SchemaId::new(CORE_WORKSPACE_RUN_WHOLE_SCHEMA.into()),
            schema_version: SchemaVersion::new(1),
        },
    }
}
