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

use crate::{AuthPath, GroupId, OwnerRef, SourceId, UserId};

/// The entity to erase under compliance.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ComplianceEraseTarget {
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

/// The owner to export under compliance access/portability.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ComplianceExportTarget {
    /// Export a group owner bundle.
    GroupOwner { group_id: GroupId },
    /// Export a personal owner bundle.
    PersonalOwner { user_id: UserId },
}

impl ComplianceExportTarget {
    /// Return the concrete owner for this export target.
    #[must_use]
    pub const fn owner(&self) -> OwnerRef {
        match self {
            Self::GroupOwner { group_id } => OwnerRef::Group(*group_id),
            Self::PersonalOwner { user_id } => OwnerRef::Personal(*user_id),
        }
    }

    /// Return the erase-family target used for controller authorization.
    ///
    /// Export is non-destructive: personal-owner export does not require drop
    /// proof, but it does require the same controller authority family as erase.
    #[must_use]
    pub fn erase_authority_target(&self) -> ComplianceEraseTarget {
        match self {
            Self::GroupOwner { group_id } => ComplianceEraseTarget::GroupOwner {
                group_id: *group_id,
            },
            Self::PersonalOwner { user_id } => ComplianceEraseTarget::PersonalOwner {
                user_id: *user_id,
                drop_event_id: String::new(),
            },
        }
    }
}

/// A request to export one owner's compliance bundle.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ComplianceExportRequest {
    /// The target owner to export.
    pub target: ComplianceExportTarget,
}

/// Counts of rows included in a compliance export bundle.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ComplianceExportCounts {
    pub memories: usize,
    pub goals: usize,
    pub edges: usize,
    pub receipts: usize,
    pub source_batches: usize,
    pub source_cursors: usize,
    #[serde(default)]
    pub delegated_authority_grants: usize,
    /// Cooled admissions, exported as locator metadata only (see
    /// [`ComplianceExportBundle::cooled`]).
    #[serde(default)]
    pub cooled: usize,
    #[serde(default)]
    pub sketches: usize,
    /// Authoritative cited-object identities exported from `proxima_core.blob`.
    #[serde(default)]
    pub blobs: usize,
    pub sidecar_rows: usize,
    pub compliance_audit_rows: usize,
}

/// JSON rows exported from one sidecar table.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ComplianceExportSidecarRows {
    pub table: String,
    pub rows: Vec<serde_json::Value>,
}

/// Owner-scoped compliance export bundle.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ComplianceExportBundle {
    pub operation_id: uuid::Uuid,
    pub target: ComplianceExportTarget,
    pub owner: OwnerRef,
    pub derived_requester: Option<UserId>,
    pub derived_auth_path: String,
    pub exported_at: time::OffsetDateTime,
    pub counts: ComplianceExportCounts,
    pub memories: Vec<serde_json::Value>,
    pub goals: Vec<serde_json::Value>,
    pub edges: Vec<serde_json::Value>,
    pub receipts: Vec<serde_json::Value>,
    pub source_batches: Vec<serde_json::Value>,
    pub source_cursors: Vec<serde_json::Value>,
    #[serde(default)]
    pub delegated_authority_grants: Vec<serde_json::Value>,
    /// Cooled admissions of the owner: one row per admission whose content
    /// left `memory` for cold storage, carrying the `object_key` that locates
    /// the dumped payload. A manifest, not the payload — the bundle stays a
    /// database export and never streams object-store bytes.
    #[serde(default)]
    pub cooled: Vec<serde_json::Value>,
    /// The owner's derived one-liners, minus the generated `search_tsv`
    /// lexical-index column.
    #[serde(default)]
    pub sketches: Vec<serde_json::Value>,
    /// The owner's authoritative cited-object identities. Each row contains
    /// only `blob_id`, `schema_id`, and `content_hash`; upload coordinates and
    /// object-store bytes are outside the compliance bundle.
    #[serde(default)]
    pub blobs: Vec<serde_json::Value>,
    /// Registered memory, goal, cited-object, and citation-mapping sidecar
    /// rows. Citation sidecars are owner-filtered through
    /// `proxima_core.blob`, the row a v0.0.8 citation is.
    pub sidecars: Vec<ComplianceExportSidecarRows>,
    pub compliance_audit_rows: Vec<serde_json::Value>,
}

impl ComplianceExportBundle {
    /// Serialize the bundle to recursively sorted-key JSON bytes.
    ///
    /// # Errors
    ///
    /// Returns a serde error if the bundle cannot be represented as JSON.
    pub fn canonical_json_bytes(&self) -> Result<Vec<u8>, serde_json::Error> {
        let value = serde_json::to_value(self)?;
        Ok(crate::canonical_json_bytes(&value))
    }
}

/// Counts of rows erased by a compliance operation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ComplianceEraseCounts {
    pub memories: u64,
    pub goals: u64,
    /// Owner-authored wake configuration rows destroyed (`prompt`,
    /// `tool_ids`, `hard_memory_t`).
    #[serde(default)]
    pub wake_configs: u64,
    /// Cited-blob rows destroyed (`schema_id`, `content_hash`).
    #[serde(default)]
    pub blobs: u64,
    /// Blob upload records destroyed (`bucket`, `object_key`, `filename`,
    /// `mime`, `sha256`, `etag`, `error_message`).
    #[serde(default)]
    pub blob_uploads: u64,
    /// Registered sidecar rows destroyed across all four families: memory,
    /// goal, cited-object, and citation-mapping.
    #[serde(default)]
    pub sidecar_rows: u64,
    pub edges: u64,
    pub receipts: u64,
    pub source_batches: u64,
    pub source_cursors: u64,
    pub embeddings: u64,
    pub embedding_jobs: u64,
    pub mcp_call_rows: u64,
    pub change_events: u64,
    pub redacted_edge_targets: u64,
    pub suppressed_keys: u64,
    #[serde(default)]
    pub delegated_authority_grants: u64,
}

/// The outcome of a compliance erase operation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ComplianceEraseOutcome {
    /// Erasure completed successfully.
    Completed {
        operation_id: uuid::Uuid,
        counts: ComplianceEraseCounts,
        /// Postgres rows are deleted but cited-object purge in the wired object
        /// store failed or was not attempted. Operators must retry purge
        /// out-of-band before treating erasure as fully complete.
        #[serde(default)]
        cited_object_purge_pending: bool,
        /// Postgres rows are deleted but one or more exact cold/object-store
        /// keys still have a durable purge debt.
        #[serde(default)]
        cold_object_purge_pending: bool,
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

/// Internal audit context for a compliance export operation.
/// Derived by `Engine` from `AuthzContext`; never caller-supplied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComplianceExportAuditContext {
    operation_id: uuid::Uuid,
    target: ComplianceExportTarget,
    /// Derived by `Engine` from `AuthzContext`; never caller-supplied.
    derived_requester: Option<UserId>,
    /// Derived by `Engine` from `AuthzContext`; never caller-supplied.
    derived_auth_path: AuthPath,
    requested_at: time::OffsetDateTime,
}

impl ComplianceExportAuditContext {
    /// Create a new export audit context.
    pub(crate) fn new(
        operation_id: uuid::Uuid,
        target: ComplianceExportTarget,
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

    /// Return the export target.
    #[must_use]
    pub fn target(&self) -> &ComplianceExportTarget {
        &self.target
    }

    /// Return the concrete exported owner.
    #[must_use]
    pub fn owner(&self) -> OwnerRef {
        self.target.owner()
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

/// Non-forgeable authorization for compliance export.
/// Callers cannot construct this; Engine creates it internally.
#[derive(Debug)]
pub struct ExportAuthorization {
    audit: ComplianceExportAuditContext,
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

    /// Test-only constructor. Engine remains the production mint.
    #[cfg(any(test, feature = "test-fixtures"))]
    #[must_use]
    pub fn new_for_tests(target: ComplianceEraseTarget) -> Self {
        Self::new(ComplianceAuditContext::new(
            uuid::Uuid::now_v7(),
            target,
            None,
            AuthPath::HostBearer,
            time::OffsetDateTime::now_utc(),
        ))
    }
}

impl ExportAuthorization {
    /// Return the audit context.
    #[must_use]
    pub const fn audit(&self) -> &ComplianceExportAuditContext {
        &self.audit
    }

    /// Create a new export authorization (internal only).
    pub(crate) fn new(audit: ComplianceExportAuditContext) -> Self {
        Self {
            audit,
            _private: private::Seal,
        }
    }

    /// Test-only constructor. Engine remains the production mint.
    #[cfg(any(test, feature = "test-fixtures"))]
    #[must_use]
    pub fn new_for_tests(target: ComplianceExportTarget) -> Self {
        Self::new(ComplianceExportAuditContext::new(
            uuid::Uuid::now_v7(),
            target,
            None,
            AuthPath::HostBearer,
            time::OffsetDateTime::now_utc(),
        ))
    }
}
