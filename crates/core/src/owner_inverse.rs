//! Owner erase API — abandonment-only hard deletion.
//!
//! Public callers can build [`OwnerEraseRequest`] and inspect
//! [`OwnerEraseOutcome`]. They cannot supply `operation_id`, requester/
//! auth-path audit identity, or deletion witnesses.
//!
//! `Engine` mints a fresh `operation_id = Uuid::now_v7()` for every erase
//! attempt, derives requester/auth path/request time from [`crate::AuthzContext`],
//! creates [`EraseAuthorization`], and `PG` still rechecks abandonment in the
//! delete transaction.

use std::collections::BTreeMap;

use crate::flavor::{EraseLeg, ForgetLeg, Surface, TransferLeg};
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
    legs: BTreeMap<&'static str, EraseLeg>,
    transfer_legs: BTreeMap<&'static str, TransferLeg>,
    forget_legs: BTreeMap<&'static str, ForgetLeg>,
}

impl OwnerSurfaces {
    /// Read every flavor's declared surfaces off one frozen registry.
    ///
    /// The engine calls this; so should anything else that needs the set,
    /// because assembling the legs by hand is what let them disagree.
    ///
    /// Each surface's [`EraseLeg`] is resolved HERE, where the surface and
    /// the flavor that declared it are both in scope, because a bespoke leg
    /// is a per-flavor declaration and the flattened set has no flavor left
    /// in it. The erase then reads the answer instead of re-deriving one.
    #[must_use]
    pub fn for_registry(registry: &crate::FlavorRegistryFrozen) -> Self {
        let mut surfaces = Vec::new();
        let mut legs = BTreeMap::new();
        let mut transfer_legs = BTreeMap::new();
        let mut forget_legs = BTreeMap::new();
        for contract in registry.contracts() {
            for surface in contract.all_surfaces() {
                legs.insert(surface.table, contract.erase_leg(&surface));
                transfer_legs.insert(surface.table, contract.transfer_leg(&surface));
                forget_legs.insert(surface.table, ForgetLeg::derive(&surface));
                surfaces.push(surface);
            }
        }
        surfaces.sort_by_key(|surface| surface.table);
        surfaces.dedup_by_key(|surface| surface.table);
        Self {
            surfaces,
            legs,
            transfer_legs,
            forget_legs,
        }
    }

    /// Build a set from surfaces given directly, with no bespoke legs.
    ///
    /// The seam a test uses to exercise a shape core declares no instance of
    /// — blob-keyed citation sidecars, or a second `RetainAtSource` table —
    /// without registering a whole flavor. Production reaches for
    /// [`Self::for_registry`]. Every surface here classifies against an
    /// EMPTY bespoke list, which is the honest answer: a surface no contract
    /// declares has no flavor to have exempted it.
    #[must_use]
    pub fn from_surfaces(mut surfaces: Vec<Surface>) -> Self {
        surfaces.sort_by_key(|surface| surface.table);
        surfaces.dedup_by_key(|surface| surface.table);
        let legs = surfaces
            .iter()
            .map(|surface| (surface.table, EraseLeg::derive(surface, &[])))
            .collect();
        let transfer_legs = surfaces
            .iter()
            .map(|surface| (surface.table, TransferLeg::derive(surface, &[])))
            .collect();
        let forget_legs = surfaces
            .iter()
            .map(|surface| (surface.table, ForgetLeg::derive(surface)))
            .collect();
        Self {
            surfaces,
            legs,
            transfer_legs,
            forget_legs,
        }
    }

    /// Every declared surface, ordered by table name.
    #[must_use]
    pub fn surfaces(&self) -> &[Surface] {
        &self.surfaces
    }

    /// Which leg destroys `table`'s rows, as its declaring flavor resolved
    /// it at registry time.
    ///
    /// [`EraseLeg::Unreachable`] for a table this set does not carry — the
    /// same answer as a surface nothing deletes, because to this set they
    /// are the same thing.
    #[must_use]
    pub fn erase_leg(&self, table: &str) -> EraseLeg {
        self.legs
            .get(table)
            .copied()
            .unwrap_or(EraseLeg::Unreachable)
    }

    /// Which leg moves `table`'s rows on a transfer, as its declaring
    /// flavor resolved it at registry time.
    ///
    /// [`TransferLeg::Unreachable`] for a table this set does not carry —
    /// the same answer as a surface nothing moves, because to this set they
    /// are the same thing. The transfer reads this and never re-derives:
    /// the whole point of resolving once, where the surface and the flavor
    /// that declared it are both in scope, is that the boot check and the
    /// verb cannot disagree.
    #[must_use]
    pub fn transfer_leg(&self, table: &str) -> TransferLeg {
        self.transfer_legs
            .get(table)
            .copied()
            .unwrap_or(TransferLeg::Unreachable)
    }

    /// What the forget does to `table`, as resolved at registry time.
    ///
    /// [`ForgetLeg::Unreachable`] for a table this set does not carry.
    #[must_use]
    pub fn forget_leg(&self, table: &str) -> ForgetLeg {
        self.forget_legs
            .get(table)
            .copied()
            .unwrap_or(ForgetLeg::Unreachable)
    }

    /// Every surface the forget deletes with a GENERATED statement, in table
    /// order, paired with the key column it deletes on.
    ///
    /// The `Dumped` legs are deliberately NOT here: the row stamp drives
    /// those, and the stamp is the historical record of what the dump read.
    /// What is left is the derived rows nothing stamps — the embedding
    /// triple and the sketch — which were four hand-written statements over
    /// four `proxima_core.*` literals.
    #[must_use]
    pub fn generated_forget_legs(&self) -> Vec<(&'static str, &'static str)> {
        self.surfaces
            .iter()
            .filter_map(|surface| match self.forget_leg(surface.table) {
                ForgetLeg::Deleted { key_column } => Some((surface.table, key_column)),
                _ => None,
            })
            .collect()
    }

    /// Every surface a transfer moves with a GENERATED statement, in table
    /// order, paired with the leg that moves it.
    ///
    /// The transfer's whole generic input. Ordered by table name so the
    /// statements a transfer runs are a function of the declarations and
    /// nothing else — not of hash iteration, not of declaration order
    /// inside a flavor.
    #[must_use]
    pub fn generated_transfer_legs(&self) -> Vec<(&'static str, TransferLeg)> {
        self.surfaces
            .iter()
            .filter_map(|surface| {
                let leg = self.transfer_leg(surface.table);
                matches!(
                    leg,
                    TransferLeg::Rehomed { .. } | TransferLeg::Dropped { .. }
                )
                .then_some((surface.table, leg))
            })
            .collect()
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

/// The entity an owner erase names.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum OwnerEraseTarget {
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

/// A request to perform owner erase.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OwnerEraseRequest {
    /// The target to erase.
    pub target: OwnerEraseTarget,
}

/// The owner whose bundle to export.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum OwnerExportTarget {
    /// Export a group owner bundle.
    GroupOwner { group_id: GroupId },
    /// Export a personal owner bundle.
    PersonalOwner { user_id: UserId },
}

impl OwnerExportTarget {
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
    pub fn erase_authority_target(&self) -> OwnerEraseTarget {
        match self {
            Self::GroupOwner { group_id } => OwnerEraseTarget::GroupOwner {
                group_id: *group_id,
            },
            Self::PersonalOwner { user_id } => OwnerEraseTarget::PersonalOwner {
                user_id: *user_id,
                drop_event_id: String::new(),
            },
        }
    }
}

/// A request to export one owner's owner bundle.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OwnerExportRequest {
    /// The target owner to export.
    pub target: OwnerExportTarget,
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
pub struct OwnerExportBundle {
    pub operation_id: uuid::Uuid,
    pub target: OwnerExportTarget,
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

impl OwnerExportBundle {
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
pub struct OwnerEraseCounts(BTreeMap<String, u64>);

impl OwnerEraseCounts {
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

/// The outcome of a owner erase operation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum OwnerEraseOutcome {
    /// Erasure completed successfully.
    Completed {
        operation_id: uuid::Uuid,
        counts: OwnerEraseCounts,
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
        reason: OwnerEraseRefusal,
    },
    /// Target not found.
    NotFound { operation_id: uuid::Uuid },
    /// Caller not authorized for this operation.
    Unauthorized { operation_id: uuid::Uuid },
}

/// Reasons for refusing a owner erase request.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum OwnerEraseRefusal {
    /// The owner is not abandoned (still has members for groups, or drop not verified for personal).
    OwnerNotAbandoned,
    /// The source scope's owner is still live.
    SourceScopeOwnerStillLive,
    /// Personal owner drop could not be verified.
    PersonalDropNotVerified,
    /// The required drop proof port is unavailable.
    DropProofPortUnavailable,
}

/// Internal audit context for a owner-erase operation.
/// Derived by `Engine` from `AuthzContext`; never caller-supplied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerEraseContext {
    operation_id: uuid::Uuid,
    target: OwnerEraseTarget,
    /// Derived by `Engine` from `AuthzContext`; never caller-supplied.
    derived_requester: Option<UserId>,
    /// Derived by `Engine` from `AuthzContext`; never caller-supplied.
    derived_auth_path: AuthPath,
    requested_at: time::OffsetDateTime,
}

impl OwnerEraseContext {
    /// Create a new audit context.
    pub(crate) fn new(
        operation_id: uuid::Uuid,
        target: OwnerEraseTarget,
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
    pub fn target(&self) -> &OwnerEraseTarget {
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

/// Internal audit context for a owner export operation.
/// Derived by `Engine` from `AuthzContext`; never caller-supplied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerExportContext {
    operation_id: uuid::Uuid,
    target: OwnerExportTarget,
    /// Derived by `Engine` from `AuthzContext`; never caller-supplied.
    derived_requester: Option<UserId>,
    /// Derived by `Engine` from `AuthzContext`; never caller-supplied.
    derived_auth_path: AuthPath,
    requested_at: time::OffsetDateTime,
}

impl OwnerExportContext {
    /// Create a new export audit context.
    pub(crate) fn new(
        operation_id: uuid::Uuid,
        target: OwnerExportTarget,
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
    pub fn target(&self) -> &OwnerExportTarget {
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

/// Non-forgeable authorization for owner erase.
/// Callers cannot construct this; Engine creates it internally.
#[derive(Debug)]
pub struct EraseAuthorization {
    audit: OwnerEraseContext,
    _private: private::Seal,
}

/// Non-forgeable authorization for owner export.
/// Callers cannot construct this; Engine creates it internally.
#[derive(Debug)]
pub struct ExportAuthorization {
    audit: OwnerExportContext,
    _private: private::Seal,
}

mod private {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(super) struct Seal;
}

impl EraseAuthorization {
    /// Return the audit context.
    #[must_use]
    pub const fn audit(&self) -> &OwnerEraseContext {
        &self.audit
    }

    /// Create a new erase authorization (internal only).
    pub(crate) fn new(audit: OwnerEraseContext) -> Self {
        Self {
            audit,
            _private: private::Seal,
        }
    }

    /// Test-only constructor. Engine remains the production mint.
    #[cfg(any(test, feature = "test-fixtures"))]
    #[must_use]
    pub fn new_for_tests(target: OwnerEraseTarget) -> Self {
        Self::new(OwnerEraseContext::new(
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
    pub const fn audit(&self) -> &OwnerExportContext {
        &self.audit
    }

    /// Create a new export authorization (internal only).
    pub(crate) fn new(audit: OwnerExportContext) -> Self {
        Self {
            audit,
            _private: private::Seal,
        }
    }

    /// Test-only constructor. Engine remains the production mint.
    #[cfg(any(test, feature = "test-fixtures"))]
    #[must_use]
    pub fn new_for_tests(target: OwnerExportTarget) -> Self {
        Self::new(OwnerExportContext::new(
            uuid::Uuid::now_v7(),
            target,
            None,
            AuthPath::HostBearer,
            time::OffsetDateTime::now_utc(),
        ))
    }
}
