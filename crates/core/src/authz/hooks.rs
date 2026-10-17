use std::fmt::Debug;

use crate::access::{AccessKind, EntityId, Relation};
use crate::authz::AuthzContext;
use crate::error::ProtocolError;
use crate::{GroupId, OwnerId};

/// Reason a hook denied an otherwise-allowed request.
#[derive(Debug)]
pub struct AuthzVeto(pub String);

/// Outcome reported to observers (audit). Fired on allow AND every denial.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthzOutcome {
    Allowed,
    DeniedGrant,
    DeniedVeto,
    DeniedResolution,
    DeniedInternal,
}

/// Direction of a group membership mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MembershipChange {
    Add,
    Remove,
}

/// What one gate checks on the resolved owner. Reads and writes follow one
/// rule: a role may act on `kind` when its limit for that direction is at
/// least `kind`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthzOperation {
    /// Read `kind` on the owner.
    ///
    /// Every engine read is gated, not only search. A set read (by-id and
    /// batch reads, lineage, edges, pins, sketches, counts, query) gates each
    /// owner in the caller's read set once, as `Read { kind: Fact }`, meaning
    /// "any read on this owner"; storage still applies the per-kind limits.
    /// An entry read (a citation or prior checked on a write path) gates only
    /// the entry's home owner, with the entry's real kind. A vetoed owner
    /// drops out of the read: its entries read exactly like absent ones.
    Read { kind: AccessKind },
    /// Write `kind` on the owner.
    Write { kind: AccessKind },
    /// Owner-level administration ([`crate::Role::administers`]).
    OwnerAdmin,
    /// Membership mutation audited by group, member, relation, and direction.
    Membership {
        change: MembershipChange,
        group: GroupId,
        member: OwnerId,
        relation: Relation,
    },
    /// Entity ownership move audited by entity and destination owner.
    /// There is no share: an owner transfer is the only cross-owner move,
    /// and the series leaves the source owner's view entirely.
    EntityTransfer { entity: EntityId, to_owner: OwnerId },
}

#[derive(Debug)]
pub struct AuthzInput<'a> {
    pub authz: &'a AuthzContext,
    pub requested: &'a OwnerId,
    pub resolved: &'a OwnerId,
    pub operation: AuthzOperation,
}

/// At most one per composed app. Remap requested -> target owner; MAY deny.
/// The resolved owner is still gated, so resolution cannot escalate.
pub trait OwnerResolver: Send + Sync + Debug + 'static {
    /// # Errors
    ///
    /// Returns a protocol error when the requested owner cannot be resolved.
    fn resolve(&self, authz: &AuthzContext, requested: &OwnerId) -> Result<OwnerId, ProtocolError>;
}

/// Zero or more, run in registration order. `veto` deny-only; `observe` is audit.
///
/// Reads are reported once per owner gated: a set read yields one decision
/// per owner in the caller's read set (`Read { kind: Fact }`), an entry read
/// one decision for the entry's home owner with its real kind. See
/// [`AuthzOperation::Read`]. Vetoing only some kinds on an owner is honoured
/// on entry reads; set reads are vetoed per owner, not per kind.
pub trait AuthorizationHook: Send + Sync + Debug + 'static {
    /// # Errors
    ///
    /// Returns [`AuthzVeto`] to deny an otherwise-authorized request.
    fn veto(&self, _input: &AuthzInput<'_>) -> Result<(), AuthzVeto> {
        Ok(())
    }

    fn observe(&self, _input: &AuthzInput<'_>, _outcome: AuthzOutcome) {}
}
