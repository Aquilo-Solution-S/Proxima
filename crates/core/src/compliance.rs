//! Compliance erasure API — abandonment-only hard deletion.
//!
//! Public callers can build [`ComplianceEraseRequest`] and inspect
//! [`ComplianceEraseOutcome`]. They cannot supply `operation_id`, requester/
//! auth-path audit identity, or deletion witnesses.
//!
//! `Engine` mints a fresh `operation_id = Uuid::now_v7()` for every compliance
//! attempt, derives requester/auth path/request time from [`crate::AuthzContext`],
//! creates [`EraseAuthorization`], and `PG` still rechecks abandonment in the
//! delete transaction.

use crate::{AuthPath, GroupId, SourceId, UserId};

/// The entity to erase under compliance.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ComplianceEraseTarget {
    /// Attempt to erase the world owner; always refused and audited.
    WorldOwner,
    /// Erase a group owner and all its owned rows.
    GroupOwner { group_id: GroupId },
    /// Erase a personal owner and all its owned rows.
    /// Requires host/usermanager-backed drop proof.
    PersonalOwner {
        user_id: UserId,
        drop_event_id: String,
    },
    /// Erase all rows for a specific source scope within a group owner.
    GroupSourceScope {
        group_id: GroupId,
        source_id: SourceId,
    },
    /// Erase all rows for a specific source scope within a personal owner.
    /// Requires host/usermanager-backed drop proof.
    PersonalSourceScope {
        user_id: UserId,
        source_id: SourceId,
        drop_event_id: String,
    },
}

/// A request to perform compliance erasure.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ComplianceEraseRequest {
    /// The target to erase.
    pub target: ComplianceEraseTarget,
}

/// Counts of rows erased by a compliance operation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ComplianceEraseCounts {
    pub memories: u64,
    pub goals: u64,
    pub edges: u64,
    pub fact_entities: u64,
    pub receipts: u64,
    pub source_batches: u64,
    pub citations: u64,
    pub cited_objects: u64,
    pub embeddings: u64,
    pub embedding_jobs: u64,
    pub mcp_call_rows: u64,
    pub change_events: u64,
    pub redacted_edge_targets: u64,
    pub suppressed_keys: u64,
}

/// The outcome of a compliance erase operation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ComplianceEraseOutcome {
    /// Erasure completed successfully.
    Completed {
        operation_id: uuid::Uuid,
        counts: ComplianceEraseCounts,
    },
    /// Erasure was refused due to policy.
    Refused {
        operation_id: uuid::Uuid,
        reason: ComplianceEraseRefusal,
    },
    /// Target not found.
    NotFound { operation_id: uuid::Uuid },
    /// Caller not authorized for this operation.
    Unauthorized { operation_id: uuid::Uuid },
}

/// Reasons for refusing a compliance erase request.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ComplianceEraseRefusal {
    /// The owner is not abandoned (still has members for groups, or drop not verified for personal).
    OwnerNotAbandoned,
    /// Cannot erase World owner.
    WorldOwner,
    /// The source scope's owner is still live.
    SourceScopeOwnerStillLive,
    /// Personal owner drop could not be verified.
    PersonalDropNotVerified,
    /// The required drop proof port is unavailable.
    DropProofPortUnavailable,
    /// A legal/security hold is active for the owner.
    LegalHoldActive,
}

/// Internal audit context for a compliance operation.
/// Derived by `Engine` from `AuthzContext`; never caller-supplied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComplianceAuditContext {
    operation_id: uuid::Uuid,
    target: ComplianceEraseTarget,
    /// Derived by `Engine` from `AuthzContext`; never caller-supplied.
    derived_requester: Option<UserId>,
    /// Derived by `Engine` from `AuthzContext`; never caller-supplied.
    derived_auth_path: AuthPath,
    requested_at: time::OffsetDateTime,
}

impl ComplianceAuditContext {
    /// Create a new audit context.
    pub(crate) fn new(
        operation_id: uuid::Uuid,
        target: ComplianceEraseTarget,
        derived_requester: Option<UserId>,
        derived_auth_path: AuthPath,
        requested_at: time::OffsetDateTime,
    ) -> Self {
        Self {
            operation_id,
            target,
            derived_requester,
            derived_auth_path,
            requested_at,
        }
    }

    /// Return the operation ID.
    #[must_use]
    pub fn operation_id(&self) -> uuid::Uuid {
        self.operation_id
    }

    /// Return the erase target.
    #[must_use]
    pub fn target(&self) -> &ComplianceEraseTarget {
        &self.target
    }

    /// Return the derived requester.
    #[must_use]
    pub fn derived_requester(&self) -> Option<UserId> {
        self.derived_requester
    }

    /// Return the derived auth path.
    #[must_use]
    pub fn derived_auth_path(&self) -> AuthPath {
        self.derived_auth_path
    }

    /// Return the request timestamp.
    #[must_use]
    pub fn requested_at(&self) -> time::OffsetDateTime {
        self.requested_at
    }
}

/// Non-forgeable authorization for compliance erasure.
/// Callers cannot construct this; Engine creates it internally.
#[derive(Debug)]
pub struct EraseAuthorization {
    audit: ComplianceAuditContext,
    _private: private::Seal,
}

mod private {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(super) struct Seal;
}

impl EraseAuthorization {
    /// Return the audit context.
    #[must_use]
    pub const fn audit(&self) -> &ComplianceAuditContext {
        &self.audit
    }

    /// Create a new erase authorization (internal only).
    pub(crate) fn new(audit: ComplianceAuditContext) -> Self {
        Self {
            audit,
            _private: private::Seal,
        }
    }
}
