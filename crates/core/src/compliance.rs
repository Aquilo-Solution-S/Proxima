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

use std::collections::BTreeMap;

use crate::flavor::Surface;
use crate::{AuthPath, GroupId, OwnerRef, SourceId, UserId};

/// Every relation an owner-scoped erase or export has to answer for, read
/// off the frozen flavor contracts.
///
/// This replaces five hand-assembled `Vec<String>` name lists. Those were
/// built from the schema registry — `PayloadKind` plus `sidecar_table` — and
/// from `pg_sidecar!(owner_pinned: true)`, which are two projections of the
/// contract rather than the contract. What a surface's inverse is,
/// `Surface::erase` already says; what it is keyed on, `Surface::key`
/// already says; which counter it feeds, `Surface::counter` already says.
/// The lanes now read those instead of a list written to agree with them.
///
/// The set is deduplicated by table: two schemas may share one sidecar, and
/// a table appears in the sweep once.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OwnerSurfaces {
    surfaces: Vec<Surface>,
}

impl OwnerSurfaces {
    /// Read every flavor's declared surfaces off one frozen registry.
    ///
    /// The engine calls this; so should anything else that needs the set,
    /// because assembling the legs by hand is what let them disagree.
    #[must_use]
    pub fn for_registry(registry: &crate::FlavorRegistryFrozen) -> Self {
        Self::from_surfaces(
            registry
                .contracts()
                .iter()
                .flat_map(|contract| contract.all_surfaces())
                .collect(),
        )
    }

    /// Build a set from surfaces given directly.
    ///
    /// The seam a test uses to exercise a shape core declares no instance of
    /// — blob-keyed citation sidecars, or a second `RetainAtSource` table —
    /// without registering a whole flavor. Production reaches for
    /// [`Self::for_registry`].
    #[must_use]
    pub fn from_surfaces(mut surfaces: Vec<Surface>) -> Self {
        surfaces.sort_by_key(|surface| surface.table);
        surfaces.dedup_by_key(|surface| surface.table);
        Self { surfaces }
    }

    /// Every declared surface, ordered by table name.
    #[must_use]
    pub fn surfaces(&self) -> &[Surface] {
        &self.surfaces
    }

    /// Every counter any declared surface contributes to, deduplicated and
    /// ordered. This is the receipt's key set: a count the declarations do
    /// not name cannot appear, and a counter they do name cannot be missing.
    #[must_use]
    pub fn counters(&self) -> Vec<&'static str> {
        let mut counters = self
            .surfaces
            .iter()
            .filter_map(|surface| surface.counter)
            .collect::<Vec<_>>();
        counters.sort_unstable();
        counters.dedup();
        counters
    }
}

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

/// Owner-scoped export bundle: one entry per declared exportable surface.
///
/// It used to be eleven typed `Vec<Value>` fields plus a twelve-field counts
/// struct, each of which had to be added by hand when a table joined the
/// export — which is how `cooled`, `sketches` and `blobs` arrived three
/// separate times, and how the code flavor's four detail tables never
/// arrived at all. The shape is now derived: `tables` has exactly the
/// surfaces whose [`ExportRule`](crate::flavor::ExportRule) is `Rows` or
/// `Allowlist`, and `counts` is a projection of `tables`, so a new surface
/// joins the bundle by declaring itself and nothing else.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ComplianceExportBundle {
    pub operation_id: uuid::Uuid,
    pub target: ComplianceExportTarget,
    pub owner: OwnerRef,
    pub derived_requester: Option<UserId>,
    pub derived_auth_path: String,
    pub exported_at: time::OffsetDateTime,
    /// Row counts, DERIVED: one entry per `tables` key, plus `edges`. A
    /// count that disagrees with the rows beside it is not representable.
    pub counts: BTreeMap<String, usize>,
    /// Table name → its rows, in the surface's declared key order. Every
    /// exportable surface is present, including the ones that came back
    /// empty: absence in the bundle would otherwise be indistinguishable
    /// from a surface the export forgot.
    pub tables: BTreeMap<String, Vec<serde_json::Value>>,
    /// Pins projected from the exported `proxima_core.memory` rows'
    /// `origins` and `refs` arrays. Not a surface — there is no edge table —
    /// so it stays its own field.
    pub edges: Vec<serde_json::Value>,
}

impl ComplianceExportBundle {
    /// The rows exported from one table, or an empty slice when the table is
    /// not part of the bundle.
    #[must_use]
    pub fn table(&self, table: &str) -> &[serde_json::Value] {
        self.tables.get(table).map_or(&[], Vec::as_slice)
    }

    /// The count recorded under `key`, or zero.
    #[must_use]
    pub fn count(&self, key: &str) -> usize {
        self.counts.get(key).copied().unwrap_or_default()
    }

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

/// The receipt of one erase: what it destroyed, per declared counter.
///
/// It used to be seventeen `u64` fields, and the fixed shape was the defect.
/// A surface declaring a NEW counter had nowhere to put it — the code
/// flavor's `repo_rows` and `ingestion_run_rows` were tallied into a temp
/// table and dropped on the floor — while four fields (`edges`,
/// `source_batches`, `redacted_edge_targets`, `suppressed_keys`) counted
/// things v0.0.8 does not have and reported a structural zero forever.
/// `sketches` was recorded and then not read back, so an erase that
/// destroyed a hundred one-liners said nothing about them.
///
/// The key set is now DERIVED: exactly the `counter` names the frozen
/// contracts declare, seeded to zero before the first delete so a declared
/// counter is present whether or not its leg ran. That is what makes the
/// receipt COMPLETE — the host that must answer for the erase gets the
/// whole tally, not the subset a struct definition anticipated.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct ComplianceEraseCounts(BTreeMap<String, u64>);

impl ComplianceEraseCounts {
    #[must_use]
    pub fn new(counts: BTreeMap<String, u64>) -> Self {
        Self(counts)
    }

    /// The tally under `name`, or zero. Zero and absent are the same answer
    /// on purpose: a counter no contract declares counted nothing.
    #[must_use]
    pub fn get(&self, name: &str) -> u64 {
        self.0.get(name).copied().unwrap_or_default()
    }

    /// Every counter in the receipt, sorted by name.
    pub fn iter(&self) -> impl Iterator<Item = (&str, u64)> {
        self.0.iter().map(|(name, count)| (name.as_str(), *count))
    }

    /// The number of distinct counters. A receipt over an empty registry is
    /// empty; the erase verbs never produce one.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Total rows destroyed across every counter.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.0.values().copied().fold(0, u64::saturating_add)
    }
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
