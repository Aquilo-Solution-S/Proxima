use std::collections::HashMap;

use proxiima_core::spiice::SchemaRef;
use proxiima_core::verbs::fact_ingest::FactWriteCommand;
use proxiima_core::verbs::goal_write::GoalPayloadWrite;
use proxiima_core::{
    AbstractionPayload, EntityKind, FactPayload, GoalId, MemoryId, OwnerId, PerspectivePayload,
    SidecarPayload, StorageError,
};
use uuid::Uuid;

use super::refused;
use super::rows::{MigrationBlob, MigrationGoal, MigrationMemory};

/// The records of one owner, including forgotten and superseded admissions.
/// Prior steps' explicit identity mappings are available for dependent spiices.
///
/// A transformation cannot shrink core's source set to evade complete coverage.
///
/// ```compile_fail
/// fn discard(input: &mut proxiima_storage_pg::MigrationInput) {
///     input.memories.clear();
/// }
/// ```
#[derive(Debug)]
pub struct MigrationInput {
    pub(crate) owner: OwnerId,
    pub(crate) memories: Vec<MigrationMemory>,
    pub(crate) goals: Vec<MigrationGoal>,
    pub(crate) blobs: Vec<MigrationBlob>,
    pub(crate) mapped: HashMap<Uuid, Vec<Uuid>>,
    pub(crate) mapped_series: HashMap<Uuid, Vec<Uuid>>,
}

impl MigrationInput {
    #[must_use]
    pub fn owner(&self) -> &OwnerId {
        &self.owner
    }
    #[must_use]
    pub fn memories(&self) -> &[MigrationMemory] {
        &self.memories
    }
    #[must_use]
    pub fn goals(&self) -> &[MigrationGoal] {
        &self.goals
    }
    #[must_use]
    pub fn blobs(&self) -> &[MigrationBlob] {
        &self.blobs
    }
    /// A declared prior-step replacement. Absence means that no preceding
    /// step has replaced this identity; it does not choose a split's meaning.
    #[must_use]
    pub fn mapped_targets(&self, source: Uuid) -> Option<&[Uuid]> {
        self.mapped.get(&source).map(Vec::as_slice)
    }
    /// Explicit preceding series mappings, separate from admission IDs.
    #[must_use]
    pub fn mapped_series_targets(&self, source: Uuid) -> Option<&[Uuid]> {
        self.mapped_series.get(&source).map(Vec::as_slice)
    }
    /// Resolve a one-to-one parent/series mapping.
    ///
    /// # Errors
    /// A split requires the spiice to select its intended successor series.
    pub fn mapped_series_one(&self, source: Uuid) -> Result<Uuid, StorageError> {
        match self.mapped_series_targets(source) {
            None => Ok(source),
            Some([target]) => Ok(*target),
            Some(_) => Err(refused(
                "a split series requires the spiice to choose its target",
            )),
        }
    }
    /// Resolve a one-to-one dependency, refusing to guess a split target.
    ///
    /// # Errors
    ///
    /// Returns an error when the source was split into multiple targets.
    pub fn mapped_one(&self, source: Uuid) -> Result<Uuid, StorageError> {
        match self.mapped_targets(source) {
            None => Ok(source),
            Some([target]) => Ok(*target),
            Some(_) => Err(refused(
                "a split reference requires the spiice to choose its target",
            )),
        }
    }
}

/// Admission identity is either retained or minted by core for this migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationAdmission {
    Retain,
    New,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationSeries {
    Retain,
    New,
    /// Select a participating series or a handle reserved by `new_series`.
    Use(Uuid),
}

/// An explicit identity/series choice; no business occurrence is published.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MigrationPlacement {
    pub admission: MigrationAdmission,
    pub series: MigrationSeries,
}

impl MigrationPlacement {
    pub const RETAIN: Self = Self {
        admission: MigrationAdmission::Retain,
        series: MigrationSeries::Retain,
    };
    pub const NEW: Self = Self {
        admission: MigrationAdmission::New,
        series: MigrationSeries::New,
    };
}

/// Core-owned pins carried beside the typed payload's own reference fields.
#[derive(Debug, Clone, Default)]
pub struct MigrationPins {
    pub references: Vec<MemoryId>,
    pub goal_references: Vec<GoalId>,
    pub origins: Vec<MemoryId>,
    pub guided_by: Vec<GoalId>,
    pub forked_from: Option<MemoryId>,
    pub blob_id: Option<Uuid>,
}

#[derive(Debug, Clone, Default)]
pub struct MigrationGoalPins {
    pub parent_handle: Option<Uuid>,
    pub close_fact: Option<MemoryId>,
    pub assignment: Option<MemoryId>,
    pub dependencies: Vec<GoalId>,
    pub evidence: Vec<MemoryId>,
    pub write_act: Option<MemoryId>,
    pub motivated_by: Vec<MemoryId>,
}

#[derive(Debug)]
pub(crate) struct MemoryTarget {
    pub source: MigrationMemory,
    pub id: Uuid,
    pub series: Uuid,
    pub payload: SidecarPayload,
    pub ingest_key: Option<String>,
    pub pins: MigrationPins,
}
#[derive(Debug)]
pub(crate) struct GoalTarget {
    pub source: MigrationGoal,
    pub id: Uuid,
    pub series: Uuid,
    pub payload: GoalPayloadWrite,
    pub pins: MigrationGoalPins,
}
#[derive(Debug)]
pub(crate) struct BlobTarget {
    pub source: MigrationBlob,
    pub id: Uuid,
    pub schema: SchemaRef,
}

/// Mapping a source to a primary target makes replay deterministic; additional
/// targets express splitting without turning one receipt into several answers.
#[derive(Debug, Clone)]
pub(crate) struct IdentityMapping {
    pub primary: Uuid,
    pub additional: Vec<Uuid>,
}
impl IdentityMapping {
    pub fn targets(&self) -> impl Iterator<Item = Uuid> + '_ {
        std::iter::once(self.primary).chain(self.additional.iter().copied())
    }
}

/// An uncommitted plan. Only the runner can construct it or persist it.
///
/// ```compile_fail
/// let plan = proxiima_storage_pg::MigrationPlan::new(
///     proxiima_core::OwnerId::new(uuid::Uuid::nil()), 0);
/// ```
#[derive(Debug)]
pub struct MigrationPlan {
    pub(crate) owner: OwnerId,
    store_number: u16,
    pub(crate) memories: Vec<MemoryTarget>,
    pub(crate) goals: Vec<GoalTarget>,
    pub(crate) blobs: Vec<BlobTarget>,
    pub(crate) mappings: HashMap<Uuid, IdentityMapping>,
    /// Explicit old/new tool fingerprint pairs. Raw business arguments are
    /// intentionally absent from core's durable request ledger.
    pub(crate) fingerprints: Vec<(String, String, String)>,
}

impl MigrationPlan {
    pub(crate) fn new(owner: OwnerId, store_number: u16) -> Self {
        Self {
            owner,
            store_number,
            memories: Vec::new(),
            goals: Vec::new(),
            blobs: Vec::new(),
            mappings: HashMap::new(),
            fingerprints: Vec::new(),
        }
    }

    /// Reserve a store-encoded series shared by several migration outputs.
    ///
    /// # Panics
    /// Has the clock bounds of [`proxiima_core::new_memory_uuid`].
    #[must_use]
    pub fn new_series(&self) -> Uuid {
        proxiima_core::new_memory_uuid(self.store_number)
    }
    fn place(
        &self,
        source: Uuid,
        series: Uuid,
        placement: MigrationPlacement,
    ) -> Result<(Uuid, Uuid), StorageError> {
        let id = match placement.admission {
            MigrationAdmission::Retain => source,
            MigrationAdmission::New => {
                proxiima_core::new_memory_uuid_after(self.store_number, source)
                    .ok_or_else(|| refused("migration admission clock exhausted"))?
            }
        };
        let series = match placement.series {
            MigrationSeries::Retain => series,
            MigrationSeries::New => self.new_series(),
            MigrationSeries::Use(series) => series,
        };
        Ok((id, series))
    }
    fn memory(
        &mut self,
        source: &MigrationMemory,
        payload: SidecarPayload,
        key: Option<String>,
        placement: MigrationPlacement,
        pins: MigrationPins,
    ) -> Result<MemoryId, StorageError> {
        let (id, series) = self.place(source.id().into_inner(), source.series(), placement)?;
        self.memories.push(MemoryTarget {
            source: source.clone(),
            id,
            series,
            payload,
            ingest_key: key,
            pins,
        });
        Ok(MemoryId::new(id))
    }
    /// Stage a target Fact, deriving its replay key through the ordinary typed
    /// Fact constructor. The occurrence time and attribution come from source.
    /// # Errors
    ///
    /// Returns an error when the source is not a Fact or the admission clock is exhausted.
    pub fn fact<P: FactPayload>(
        &mut self,
        source: &MigrationMemory,
        payload: P,
        placement: MigrationPlacement,
        pins: MigrationPins,
    ) -> Result<MemoryId, StorageError> {
        if source.kind() != EntityKind::Fact {
            return Err(refused("schema migration cannot change a memory's layer"));
        }
        let key =
            source.row.source_id.as_ref().and_then(|source_id| {
                FactWriteCommand::from_payload(source_id, &payload).ingest_key
            });
        self.memory(source, SidecarPayload::fact(payload), key, placement, pins)
    }
    /// # Errors
    ///
    /// Returns an error when the source is not an Abstraction or the admission clock is exhausted.
    pub fn abstraction<P: AbstractionPayload>(
        &mut self,
        source: &MigrationMemory,
        payload: P,
        placement: MigrationPlacement,
        pins: MigrationPins,
    ) -> Result<MemoryId, StorageError> {
        if source.kind() != EntityKind::Abstraction {
            return Err(refused("schema migration cannot change a memory's layer"));
        }
        self.memory(
            source,
            SidecarPayload::abstraction(payload),
            None,
            placement,
            pins,
        )
    }
    /// # Errors
    ///
    /// Returns an error when the source is not a Perspective or the admission clock is exhausted.
    pub fn perspective<P: PerspectivePayload>(
        &mut self,
        source: &MigrationMemory,
        payload: P,
        placement: MigrationPlacement,
        pins: MigrationPins,
    ) -> Result<MemoryId, StorageError> {
        if source.kind() != EntityKind::Perspective {
            return Err(refused("schema migration cannot change a memory's layer"));
        }
        self.memory(
            source,
            SidecarPayload::perspective(payload),
            None,
            placement,
            pins,
        )
    }
    /// The common representation-only operation preserves both admission and
    /// series and completes the source's replay mapping in the same call.
    /// # Errors
    ///
    /// Returns an error when the source is not a Fact or has already been mapped.
    pub fn retain_fact<P: FactPayload>(
        &mut self,
        source: &MigrationMemory,
        payload: P,
    ) -> Result<MemoryId, StorageError> {
        let target = self.fact(source, payload, MigrationPlacement::RETAIN, source.pins())?;
        self.map_memory(source, target, &[])?;
        Ok(target)
    }
    /// # Errors
    ///
    /// Returns an error when the admission clock is exhausted.
    pub fn goal(
        &mut self,
        source: &MigrationGoal,
        payload: GoalPayloadWrite,
        placement: MigrationPlacement,
        pins: MigrationGoalPins,
    ) -> Result<GoalId, StorageError> {
        let (id, series) = self.place(source.id().into_inner(), source.series(), placement)?;
        self.goals.push(GoalTarget {
            source: source.clone(),
            id,
            series,
            payload,
            pins,
        });
        Ok(GoalId::new(id))
    }
    /// Change a cited object's schema while retaining its immutable bytes.
    pub fn blob(
        &mut self,
        source: &MigrationBlob,
        schema: SchemaRef,
        identity: MigrationAdmission,
    ) -> Uuid {
        let id = match identity {
            MigrationAdmission::Retain => source.id(),
            MigrationAdmission::New => Uuid::new_v4(),
        };
        self.blobs.push(BlobTarget {
            source: source.clone(),
            id,
            schema,
        });
        id
    }
    fn map(
        &mut self,
        source: Uuid,
        primary: Uuid,
        additional: Vec<Uuid>,
    ) -> Result<(), StorageError> {
        match self.mappings.entry(source) {
            std::collections::hash_map::Entry::Occupied(_) => {
                Err(refused("migration source was mapped more than once"))
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(IdentityMapping {
                    primary,
                    additional,
                });
                Ok(())
            }
        }
    }
    /// # Errors
    ///
    /// Returns an error when the source was already mapped.
    pub fn map_memory(
        &mut self,
        source: &MigrationMemory,
        primary: MemoryId,
        additional: &[MemoryId],
    ) -> Result<(), StorageError> {
        self.map(
            source.id().into_inner(),
            primary.into_inner(),
            additional.iter().map(|id| id.into_inner()).collect(),
        )
    }
    /// # Errors
    ///
    /// Returns an error when the source was already mapped.
    pub fn map_goal(
        &mut self,
        source: &MigrationGoal,
        primary: GoalId,
        additional: &[GoalId],
    ) -> Result<(), StorageError> {
        self.map(
            source.id().into_inner(),
            primary.into_inner(),
            additional.iter().map(|id| id.into_inner()).collect(),
        )
    }
    /// # Errors
    ///
    /// Returns an error when the source was already mapped.
    pub fn map_blob(
        &mut self,
        source: &MigrationBlob,
        primary: Uuid,
        additional: &[Uuid],
    ) -> Result<(), StorageError> {
        self.map(source.id(), primary, additional.to_vec())
    }
    /// Supply an explicitly reconstructed tool fingerprint when its argument
    /// encoding changes. The runner requires the old ledger entry to exist.
    pub fn map_request_fingerprint(
        &mut self,
        source: impl Into<String>,
        old: impl Into<String>,
        new: impl Into<String>,
    ) {
        self.fingerprints
            .push((source.into(), old.into(), new.into()));
    }
}

#[cfg(test)]
mod tests {
    use super::MigrationPlan;
    use proxiima_core::OwnerId;
    use uuid::Uuid;

    #[test]
    fn reserved_series_routes_to_the_migrating_store() {
        let plan = MigrationPlan::new(OwnerId::new(Uuid::new_v4()), 42);
        assert_eq!(proxiima_core::memory_store_number(plan.new_series()), 42);
    }
}
