//! Backend-neutral read DTOs shared by engine and storage ports.

use crate::{
    ChangeEvent, EntityKind, GoalId, MemoryId, OwnerRef, SchemaId, SchemaVersion, SidecarPayload,
    ToolScope,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactRow {
    pub memory_id: MemoryId,
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    pub payload: Option<SidecarPayload>,
}

/// Persisted recall/think one-liner. Plumbing; not a kernel sort.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemorySketch {
    pub id: MemoryId,
    pub owner: OwnerRef,
    pub kind: EntityKind,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemorySnapshot {
    pub memory_id: MemoryId,
    pub kind: EntityKind,
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    pub owner: OwnerRef,
    pub text: Option<String>,
    pub payload: Option<SidecarPayload>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbstractionRow {
    pub memory_id: MemoryId,
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    pub text: String,
    pub payload: Option<SidecarPayload>,
}

#[derive(Debug, Clone)]
pub struct SidecarSpec {
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    pub sidecar_table: String,
}

/// Triage-level summary of one active Goal. Detail is reachable through
/// the Goal read/query surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveGoalSummary {
    pub goal_id: GoalId,
    pub goal_activated_memory_id: Option<MemoryId>,
    pub title: String,
}

/// Change event row selected by wake candidate/admission reads. It is not
/// an executable wake entry and carries no actor authority.
#[derive(Debug, Clone)]
pub struct ChangeEventForWake {
    pub event: ChangeEvent,
}

/// Actor-resolved wake candidate query. The caller supplies already authorized
/// owner sets; stored Goal wake config only narrows these grants.
#[derive(Debug, Clone, Copy)]
pub struct GoalWakeCandidateRequest<'a> {
    pub actor_read_owners: &'a [OwnerRef],
    pub actor_write_owners: &'a [OwnerRef],
    pub trigger_owner: OwnerRef,
    pub trigger_fact_id: MemoryId,
    pub trigger_schema_id: &'a SchemaId,
    pub trigger_schema_version: SchemaVersion,
    pub actor_tool_scope: &'a ToolScope,
    pub deployment_tool_scope: &'a ToolScope,
    pub limit: usize,
}

/// One armed Active Goal admitted for wake planning. This is a read model only:
/// PR6 has no executor, tool invocation row, or emitted Fact write path here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalWakeCandidate {
    pub goal_id: GoalId,
    pub tool_ids: Vec<String>,
    pub prompt: String,
    pub hard_memories: Vec<GoalWakeHardMemory>,
    pub actor_write_owners: Vec<OwnerRef>,
}

/// One pinned wake-context memory with the kind needed to render a
/// class-correct reference (`F:`/`A:`/`P:`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GoalWakeHardMemory {
    pub memory_id: MemoryId,
    pub kind: EntityKind,
}

/// One goal's stored wake configuration, read back for introspection
/// (`proxima://goal/{id}` / `proxima://goals`). Exactly one trigger class
/// is populated: a concrete trigger Fact (`trigger_memory_id`) or a
/// schema trigger (`trigger_schema_id` + optional version).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalWakeConfigRow {
    pub goal_id: GoalId,
    pub trigger_memory_id: Option<MemoryId>,
    pub trigger_schema_id: Option<SchemaId>,
    pub trigger_schema_version: Option<SchemaVersion>,
    pub tool_ids: Vec<String>,
    pub prompt: String,
    pub hard_memories: Vec<GoalWakeHardMemory>,
}
