use proxima_core::verbs::goal_write::GoalState;
use proxima_core::verbs::query::{GoalRow, MemoryRow};
use proxima_core::{
    GoalId, MemoryId, OwnerId, SchemaId, SchemaVersion, SidecarPayload, StorageError,
};

pub(super) fn memory_row_from_db(
    r: MemoryRowDb,
    payload: Option<SidecarPayload>,
    schema_version: SchemaVersion,
) -> Result<MemoryRow, StorageError> {
    let schema_id = SchemaId::new(r.schema_id);

    Ok(MemoryRow {
        handle: r.handle,
        id: MemoryId::new(r.memory_id),
        kind: parse_memory_kind(&r.kind)?,
        schema_id,
        schema_version,
        owner: owner_from_parts(r.owner_id)?,
        origins: r.origins.into_iter().map(MemoryId::new).collect(),
        refs: r.refs.into_iter().map(MemoryId::new).collect(),
        goal_refs: r.goal_refs.into_iter().map(GoalId::new).collect(),
        occurred_at: r.occurred_at,
        payload,
    })
}

pub(super) fn goal_row_from_db(r: GoalRowDb) -> Result<GoalRow, StorageError> {
    Ok(GoalRow {
        handle: r.handle,
        id: GoalId::new(r.goal_id),
        schema_id: SchemaId::new(r.schema_id),
        owner: owner_from_parts(r.owner_id)?,
        title: r.title,
        text: r.text,
        state: r.state,
        dependency_goal_ids: r.dependency_goal_ids.into_iter().map(GoalId::new).collect(),
        assignment: r.assignment.map(MemoryId::new),
        evidence: r.evidence.into_iter().map(MemoryId::new).collect(),
        set_at: r.set_at,
        changed_at: r.created_at,
    })
}

fn parse_memory_kind(kind: &str) -> Result<proxima_core::EntityKind, StorageError> {
    match kind {
        "fact" | "Fact" => Ok(proxima_core::EntityKind::Fact),
        "abstraction" | "Abstraction" => Ok(proxima_core::EntityKind::Abstraction),
        "perspective" | "Perspective" => Ok(proxima_core::EntityKind::Perspective),
        other => Err(StorageError::Internal(format!(
            "invalid memory kind {other}"
        ))),
    }
}

fn owner_from_parts(owner_id: Option<uuid::Uuid>) -> Result<OwnerId, StorageError> {
    owner_id
        .map(OwnerId::new)
        .ok_or_else(|| StorageError::Internal("owner_id missing".into()))
}

#[derive(Debug, sqlx::FromRow)]
pub(super) struct GoalRowDb {
    pub(super) handle: uuid::Uuid,
    pub(super) goal_id: uuid::Uuid,
    /// This version's time: the keyset position, and the row's `changed_at`.
    pub(super) created_at: time::OffsetDateTime,
    /// The time of the series' first version.
    set_at: time::OffsetDateTime,
    schema_id: String,
    owner_id: Option<uuid::Uuid>,
    title: String,
    text: Option<String>,
    state: GoalState,
    dependency_goal_ids: Vec<uuid::Uuid>,
    assignment: Option<uuid::Uuid>,
    evidence: Vec<uuid::Uuid>,
}

#[derive(Debug, sqlx::FromRow)]
pub(super) struct MemoryRowDb {
    pub(super) memory_id: uuid::Uuid,
    pub(super) handle: uuid::Uuid,
    pub(super) created_at: time::OffsetDateTime,
    owner_id: Option<uuid::Uuid>,
    pub(super) schema_id: String,
    pub(super) sidecar_tables: Vec<String>,
    pub(super) kind: String,
    pub(super) origins: Vec<uuid::Uuid>,
    pub(super) refs: Vec<uuid::Uuid>,
    pub(super) goal_refs: Vec<uuid::Uuid>,
    occurred_at: Option<time::OffsetDateTime>,
}
