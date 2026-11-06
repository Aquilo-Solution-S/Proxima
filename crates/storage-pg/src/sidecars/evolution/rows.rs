use proxiima_core::{EntityKind, GoalId, MemoryId, OwnerId, SidecarPayload};
use uuid::Uuid;

/// One historical memory admission selected by a schema-evolution source.
#[derive(Debug, Clone)]
pub struct MigrationMemoryRow {
    pub(crate) handle: Uuid,
    pub(crate) t: MemoryId,
    pub(crate) kind: EntityKind,
    pub(crate) owner_id: OwnerId,
    pub(crate) schema_id: String,
    pub(crate) source_id: Option<String>,
    pub(crate) ingest_key: Option<String>,
    pub(crate) blob_id: Option<Uuid>,
    pub(crate) content_id: Option<Uuid>,
    pub(crate) origins: Vec<Uuid>,
    pub(crate) refs: Vec<Uuid>,
    pub(crate) goal_refs: Vec<Uuid>,
    pub(crate) guided_by: Vec<Uuid>,
    pub(crate) forked_from: Option<Uuid>,
    pub(crate) sidecar_tables: Vec<String>,
    pub(crate) occurred_at: Option<time::OffsetDateTime>,
    pub(crate) by: Uuid,
    pub(crate) via: Option<Uuid>,
    pub(crate) lexical_language: String,
    pub(crate) forgotten_at: Option<time::OffsetDateTime>,
}

impl MigrationMemoryRow {
    #[must_use]
    pub fn source_id(&self) -> Option<&str> {
        self.source_id.as_deref()
    }
    #[must_use]
    pub fn ingest_key(&self) -> Option<&str> {
        self.ingest_key.as_deref()
    }
    #[must_use]
    pub const fn content_id(&self) -> Option<Uuid> {
        self.content_id
    }
    #[must_use]
    pub const fn id(&self) -> MemoryId {
        self.t
    }
    #[must_use]
    pub const fn series(&self) -> Uuid {
        self.handle
    }
    #[must_use]
    pub const fn owner(&self) -> OwnerId {
        self.owner_id
    }
    #[must_use]
    pub const fn kind(&self) -> EntityKind {
        self.kind
    }
    #[must_use]
    pub const fn occurred_at(&self) -> Option<time::OffsetDateTime> {
        self.occurred_at
    }
    #[must_use]
    pub fn pins(&self) -> super::MigrationPins {
        super::MigrationPins {
            references: self.refs.iter().copied().map(MemoryId::new).collect(),
            goal_references: self.goal_refs.iter().copied().map(GoalId::new).collect(),
            origins: self.origins.iter().copied().map(MemoryId::new).collect(),
            guided_by: self.guided_by.iter().copied().map(GoalId::new).collect(),
            forked_from: self.forked_from.map(MemoryId::new),
            blob_id: self.blob_id,
        }
    }
}

#[derive(Debug, Clone)]
pub struct MigrationMemory {
    pub(crate) row: MigrationMemoryRow,
    pub(crate) payload: Option<SidecarPayload>,
}

#[cfg(test)]
pub(super) fn test_memory(
    owner: OwnerId,
    id: Uuid,
    kind: EntityKind,
    source_id: Option<&str>,
) -> MigrationMemory {
    MigrationMemory {
        row: MigrationMemoryRow {
            handle: id,
            t: MemoryId::new(id),
            kind,
            owner_id: owner,
            schema_id: "test/source-v1".to_owned(),
            source_id: source_id.map(str::to_owned),
            ingest_key: None,
            blob_id: None,
            content_id: Some(id),
            origins: Vec::new(),
            refs: Vec::new(),
            goal_refs: Vec::new(),
            guided_by: Vec::new(),
            forked_from: None,
            sidecar_tables: Vec::new(),
            occurred_at: None,
            by: id,
            via: None,
            lexical_language: "pg_catalog.simple".to_owned(),
            forgotten_at: None,
        },
        payload: None,
    }
}

impl MigrationMemory {
    #[must_use]
    pub const fn row(&self) -> &MigrationMemoryRow {
        &self.row
    }
    #[must_use]
    pub const fn id(&self) -> MemoryId {
        self.row.t
    }
    #[must_use]
    pub const fn series(&self) -> Uuid {
        self.row.handle
    }
    #[must_use]
    pub const fn owner(&self) -> OwnerId {
        self.row.owner_id
    }
    #[must_use]
    pub const fn kind(&self) -> EntityKind {
        self.row.kind
    }
    #[must_use]
    pub const fn occurred_at(&self) -> Option<time::OffsetDateTime> {
        self.row.occurred_at
    }
    #[must_use]
    pub fn pins(&self) -> super::MigrationPins {
        self.row.pins()
    }
    #[must_use]
    pub fn payload<P: Send + Sync + 'static>(&self) -> Option<&P> {
        self.payload.as_ref().and_then(SidecarPayload::downcast_ref)
    }
}

#[derive(Debug, Clone)]
pub struct MigrationGoalRow {
    pub(crate) handle: Uuid,
    pub(crate) t: GoalId,
    pub(crate) owner_id: OwnerId,
    pub(crate) schema_id: String,
    pub(crate) by: Uuid,
    pub(crate) via: Option<Uuid>,
    pub(crate) title: String,
    pub(crate) text: Option<String>,
    pub(crate) state: proxiima_core::verbs::goal_write::GoalState,
    pub(crate) request_id: String,
    pub(crate) close_fact_t: Option<Uuid>,
    pub(crate) assignment_t: Option<Uuid>,
    pub(crate) dependency_t: Vec<Uuid>,
    pub(crate) evidence_t: Vec<Uuid>,
    pub(crate) motivated_by: Vec<Uuid>,
    pub(crate) write_act_t: Option<Uuid>,
    pub(crate) parent_handle: Option<Uuid>,
    pub(crate) sidecar: Option<SidecarPayload>,
}

impl MigrationGoalRow {
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }
    #[must_use]
    pub fn text(&self) -> Option<&str> {
        self.text.as_deref()
    }
    #[must_use]
    pub fn request_id(&self) -> &str {
        &self.request_id
    }
    #[must_use]
    pub fn payload<P: Send + Sync + 'static>(&self) -> Option<&P> {
        self.sidecar.as_ref().and_then(SidecarPayload::downcast_ref)
    }
    #[must_use]
    pub const fn id(&self) -> GoalId {
        self.t
    }
    #[must_use]
    pub const fn series(&self) -> Uuid {
        self.handle
    }
    #[must_use]
    pub const fn owner(&self) -> OwnerId {
        self.owner_id
    }
    #[must_use]
    pub const fn state(&self) -> proxiima_core::verbs::goal_write::GoalState {
        self.state
    }
    #[must_use]
    pub const fn schema_id(&self) -> &str {
        self.schema_id.as_str()
    }
    #[must_use]
    pub fn pins(&self) -> super::MigrationGoalPins {
        super::MigrationGoalPins {
            parent_handle: self.parent_handle,
            close_fact: self.close_fact_t.map(MemoryId::new),
            assignment: self.assignment_t.map(MemoryId::new),
            dependencies: self.dependency_t.iter().copied().map(GoalId::new).collect(),
            evidence: self.evidence_t.iter().copied().map(MemoryId::new).collect(),
            write_act: self.write_act_t.map(MemoryId::new),
            motivated_by: self
                .motivated_by
                .iter()
                .copied()
                .map(MemoryId::new)
                .collect(),
        }
    }
}

pub type MigrationGoal = MigrationGoalRow;

#[derive(Debug, Clone)]
pub struct MigrationBlob {
    pub(crate) blob_id: Uuid,
    pub(crate) owner_id: OwnerId,
    pub(crate) schema_id: String,
    pub(crate) content_hash: Vec<u8>,
}

impl MigrationBlob {
    #[must_use]
    pub const fn id(&self) -> Uuid {
        self.blob_id
    }
    #[must_use]
    pub const fn owner(&self) -> OwnerId {
        self.owner_id
    }
    #[must_use]
    pub fn schema_id(&self) -> &str {
        &self.schema_id
    }
    #[must_use]
    pub fn content_hash(&self) -> &[u8] {
        &self.content_hash
    }
}

pub(crate) fn parse_memory_kind(raw: &str) -> Result<EntityKind, String> {
    match raw {
        "fact" => Ok(EntityKind::Fact),
        "abstraction" => Ok(EntityKind::Abstraction),
        "perspective" => Ok(EntityKind::Perspective),
        other => Err(format!("unknown memory kind {other:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::parse_memory_kind;
    use proxiima_core::EntityKind;

    #[test]
    fn database_memory_kind_is_closed() {
        assert_eq!(parse_memory_kind("fact"), Ok(EntityKind::Fact));
        assert!(parse_memory_kind("goal").is_err());
    }
}
