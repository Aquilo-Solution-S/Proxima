//! Typed lifecycle requests and receipts for registered host-owned state.
//!
//! These values carry scope across the core/Postgres boundary. They grant no
//! erase or export authority; the existing owner inverse authorization and
//! hard-erase transaction remain the only entry points.

use crate::{MemoryId, OwnerRef, SourceId};

use super::{HostStateParticipantId, StateSurfaceName};

/// The exact target selected by the already-authorized core owner erase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostStateEraseScope {
    WholeOwner,
    Source(SourceId),
}

/// A host callback invoked inside the active core erase transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostStateEraseRequest {
    participant: HostStateParticipantId,
    owner: OwnerRef,
    scope: HostStateEraseScope,
    /// Exact `t` values in the sealed hot ∪ cooled Fact selection. This is
    /// informational scope for the host callback, not a new core read port.
    selected_fact_ids: Vec<MemoryId>,
    declared_tables: Vec<StateSurfaceName>,
}

impl HostStateEraseRequest {
    #[must_use]
    pub fn new(
        participant: HostStateParticipantId,
        owner: OwnerRef,
        scope: HostStateEraseScope,
        selected_fact_ids: Vec<MemoryId>,
        declared_tables: Vec<StateSurfaceName>,
    ) -> Self {
        Self {
            participant,
            owner,
            scope,
            selected_fact_ids,
            declared_tables,
        }
    }

    #[must_use]
    pub const fn participant(&self) -> HostStateParticipantId {
        self.participant
    }

    #[must_use]
    pub const fn owner(&self) -> OwnerRef {
        self.owner
    }

    #[must_use]
    pub fn scope(&self) -> &HostStateEraseScope {
        &self.scope
    }

    #[must_use]
    pub fn selected_fact_ids(&self) -> &[MemoryId] {
        &self.selected_fact_ids
    }

    #[must_use]
    pub fn declared_tables(&self) -> &[StateSurfaceName] {
        &self.declared_tables
    }
}

/// Per-table deletion and scrubbing counts returned by one callback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostStateEraseTableCount {
    pub table: StateSurfaceName,
    pub deleted: u64,
    pub scrubbed: u64,
}

/// Callback receipt. Storage validates identity, exact table coverage, and
/// retain-policy counts before committing the shared erase transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostStateEraseReceipt {
    pub participant: HostStateParticipantId,
    pub owner: OwnerRef,
    pub scope: HostStateEraseScope,
    pub counts: Vec<HostStateEraseTableCount>,
}

/// Whole-owner export request for one frozen host participant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostStateExportRequest {
    participant: HostStateParticipantId,
    owner: OwnerRef,
    included_tables: Vec<StateSurfaceName>,
}

impl HostStateExportRequest {
    #[must_use]
    pub fn new(
        participant: HostStateParticipantId,
        owner: OwnerRef,
        included_tables: Vec<StateSurfaceName>,
    ) -> Self {
        Self {
            participant,
            owner,
            included_tables,
        }
    }

    #[must_use]
    pub const fn participant(&self) -> HostStateParticipantId {
        self.participant
    }

    #[must_use]
    pub const fn owner(&self) -> OwnerRef {
        self.owner
    }

    #[must_use]
    pub fn included_tables(&self) -> &[StateSurfaceName] {
        &self.included_tables
    }
}

/// Rows for one included host table. Counts are derived from `rows` by
/// storage; participants cannot submit a second, potentially divergent tally.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostStateExportTable {
    pub table: StateSurfaceName,
    pub rows: Vec<serde_json::Value>,
}

/// Whole-owner callback result. The table list is a vector so duplicate and
/// missing table names remain observable and can fail closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostStateExportReceipt {
    pub participant: HostStateParticipantId,
    pub owner: OwnerRef,
    pub tables: Vec<HostStateExportTable>,
}
