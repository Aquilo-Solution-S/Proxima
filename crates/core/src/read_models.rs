//! Backend-neutral read DTOs shared by engine and storage ports.

use crate::{
    ChangeEvent, EntityKind, GoalId, MemoryId, OwnerRef, SchemaId, SchemaVersion, SidecarPayload,
    StorageError, ToolScope,
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
pub struct MemorySchemaSpec {
    pub kind: EntityKind,
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    pub sidecar_table: Option<String>,
}

/// Resolve the single registered declaration for a persisted Memory selector.
///
/// Memory rows store `(kind, schema_id)` but no version. A missing or
/// ambiguous declaration is a registry/storage invariant failure, never a
/// reason to invent a version or omit a visible row.
///
/// # Errors
///
/// Returns [`StorageError::ConstraintViolation`] when zero or multiple
/// declarations match the stored selector.
pub fn resolve_memory_schema<'a>(
    specs: &'a [MemorySchemaSpec],
    kind: EntityKind,
    schema_id: &SchemaId,
) -> Result<&'a MemorySchemaSpec, StorageError> {
    let mut matches = specs
        .iter()
        .filter(|spec| spec.kind == kind && spec.schema_id == *schema_id);
    let Some(first) = matches.next() else {
        return Err(StorageError::ConstraintViolation(format!(
            "no registered Memory schema for {kind:?} {}",
            schema_id.as_str()
        )));
    };
    if matches.next().is_some() {
        return Err(StorageError::ConstraintViolation(format!(
            "ambiguous registered Memory schema for {kind:?} {}",
            schema_id.as_str()
        )));
    }
    Ok(first)
}

#[cfg(test)]
mod tests {
    use super::{MemorySchemaSpec, resolve_memory_schema};
    use crate::{EntityKind, SchemaId, SchemaVersion, StorageError};

    fn spec(kind: EntityKind, id: &str, version: u32) -> MemorySchemaSpec {
        MemorySchemaSpec {
            kind,
            schema_id: SchemaId::new(id.to_owned()),
            schema_version: SchemaVersion::new(version),
            sidecar_table: None,
        }
    }

    #[test]
    fn resolver_requires_exactly_one_selector() {
        let id = SchemaId::new("test/book".to_owned());
        assert!(matches!(
            resolve_memory_schema(&[], EntityKind::Fact, &id),
            Err(StorageError::ConstraintViolation(_))
        ));
        let one = [spec(EntityKind::Fact, "test/book", 2)];
        assert_eq!(
            resolve_memory_schema(&one, EntityKind::Fact, &id)
                .expect("one declaration")
                .schema_version,
            SchemaVersion::new(2)
        );
        let ambiguous = [
            spec(EntityKind::Fact, "test/book", 1),
            spec(EntityKind::Fact, "test/book", 2),
        ];
        assert!(matches!(
            resolve_memory_schema(&ambiguous, EntityKind::Fact, &id),
            Err(StorageError::ConstraintViolation(_))
        ));
    }

    #[test]
    fn resolver_allows_same_id_across_memory_layers() {
        let id = SchemaId::new("test/book".to_owned());
        let specs = [
            spec(EntityKind::Fact, "test/book", 1),
            spec(EntityKind::Abstraction, "test/book", 2),
        ];
        assert_eq!(
            resolve_memory_schema(&specs, EntityKind::Fact, &id)
                .expect("Fact declaration")
                .schema_version,
            SchemaVersion::new(1)
        );
        assert_eq!(
            resolve_memory_schema(&specs, EntityKind::Abstraction, &id)
                .expect("Abstraction declaration")
                .schema_version,
            SchemaVersion::new(2)
        );
    }
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
