//! Typed lifecycle requests and receipts for registered host-owned state.
//!
//! These values carry scope across the core/Postgres boundary. They grant no
//! erase or export authority; the existing owner inverse authorization and
//! hard-erase transaction remain the only entry points.

use crate::{MemoryId, OwnerRef, SourceId};

use super::{HostStateParticipantId, StateSurfaceName};

/// One payload copy captured when the physical Fact was originally
/// published. The current owner may differ after transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HostStateFactCopyLocator {
    pub original_owner: OwnerRef,
    pub fact_id: MemoryId,
}

/// Shared set identity carried by a physical Fact erase and its callback
/// receipt. Construction sorts and removes duplicates, so order and repeated
/// members cannot change which physical Facts or original copies are selected.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HostStateEraseSelection {
    physical_facts: Vec<MemoryId>,
    original_copies: Vec<HostStateFactCopyLocator>,
}

impl HostStateEraseSelection {
    #[must_use]
    pub fn new(
        mut physical_facts: Vec<MemoryId>,
        mut original_copies: Vec<HostStateFactCopyLocator>,
    ) -> Self {
        physical_facts.sort_unstable();
        physical_facts.dedup();
        original_copies.sort_unstable();
        original_copies.dedup();
        Self {
            physical_facts,
            original_copies,
        }
    }

    #[must_use]
    pub fn physical_facts(&self) -> &[MemoryId] {
        &self.physical_facts
    }

    #[must_use]
    pub fn original_copies(&self) -> &[HostStateFactCopyLocator] {
        &self.original_copies
    }
}

/// The exact target selected by the already-authorized core owner erase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostStateEraseScope {
    WholeOwner,
    Source(SourceId),
    /// The caller has already authorized a hard erase of exact physical
    /// Fact IDs. This scope never carries original-owner selectors.
    ExactFacts,
}

/// A host callback invoked inside the active core erase transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostStateEraseRequest {
    participant: HostStateParticipantId,
    owner: OwnerRef,
    scope: HostStateEraseScope,
    /// Exact physical Facts and immutable original-owner copies selected by
    /// the already-authorized core erase.
    selection: HostStateEraseSelection,
    declared_tables: Vec<StateSurfaceName>,
}

impl HostStateEraseRequest {
    #[must_use]
    pub fn new(
        participant: HostStateParticipantId,
        owner: OwnerRef,
        scope: HostStateEraseScope,
        selection: HostStateEraseSelection,
        declared_tables: Vec<StateSurfaceName>,
    ) -> Self {
        Self {
            participant,
            owner,
            scope,
            selection: HostStateEraseSelection::new(
                selection.physical_facts,
                selection.original_copies,
            ),
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
        self.selection.physical_facts()
    }

    #[must_use]
    pub fn selection(&self) -> &HostStateEraseSelection {
        &self.selection
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
    pub selection: HostStateEraseSelection,
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

#[cfg(test)]
mod tests {
    use super::{HostStateEraseSelection, HostStateFactCopyLocator};
    use crate::{MemoryId, OwnerRef, UserId};
    use uuid::Uuid;

    #[test]
    fn erase_selection_is_a_canonical_set() {
        let first = MemoryId::new(Uuid::now_v7());
        let second = MemoryId::new(Uuid::now_v7());
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let locator = HostStateFactCopyLocator {
            original_owner: owner,
            fact_id: first,
        };

        let left =
            HostStateEraseSelection::new(vec![second, first, second], vec![locator, locator]);
        let right = HostStateEraseSelection::new(vec![first, second], vec![locator]);

        assert_eq!(left, right);
        assert_eq!(
            left.physical_facts(),
            &[first.min(second), first.max(second)]
        );
        assert_eq!(left.original_copies(), &[locator]);
    }
}
