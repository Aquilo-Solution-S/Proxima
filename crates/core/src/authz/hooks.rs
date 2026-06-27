use std::fmt::Debug;

use crate::access::{EntityId, Relation};
use crate::authz::AuthzContext;
use crate::error::ProtocolError;
use crate::{GroupId, Owner, Principal};

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthzOperation {
    /// Ordinary relation-gated owner/space/entry access.
    Relation { relation: Relation },
    /// Membership mutation audited by group, member, and relation.
    Membership {
        group: GroupId,
        member: Principal,
        relation: Relation,
    },
    /// Entity ownership/share mutation audited by entity and owner.
    EntityShare { entity: EntityId, owner: Principal },
}

#[derive(Debug)]
pub struct AuthzInput<'a> {
    pub authz: &'a AuthzContext,
    pub requested: &'a Owner,
    pub resolved: &'a Owner,
    pub relation: Relation,
    pub operation: AuthzOperation,
}

/// At most one per composed app. Remap requested -> target owner; MAY deny.
/// The resolved owner is still gated, so resolution cannot escalate.
pub trait OwnerResolver: Send + Sync + Debug + 'static {
    /// # Errors
    ///
    /// Returns a protocol error when the requested owner cannot be resolved.
    fn resolve(&self, authz: &AuthzContext, requested: &Owner) -> Result<Owner, ProtocolError>;
}

/// Zero or more, run in registration order. `veto` deny-only; `observe` is audit.
pub trait AuthorizationHook: Send + Sync + Debug + 'static {
    /// # Errors
    ///
    /// Returns [`AuthzVeto`] to deny an otherwise-authorized request.
    fn veto(&self, _input: &AuthzInput<'_>) -> Result<(), AuthzVeto> {
        Ok(())
    }

    fn observe(&self, _input: &AuthzInput<'_>, _outcome: AuthzOutcome) {}
}
