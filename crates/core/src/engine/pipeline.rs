//! The single Tier-2 authorization chokepoint. `MemoryPermit` is sealed: its
//! only constructor is `Engine::authorize_request` in this module, so a verb
//! body that requires a permit cannot run without one and cannot forge one.

use crate::Owner;
use crate::authz::{AuthzContext, MemoryAction, Role};
use crate::error::ProtocolError;

use super::{Engine, authorize, authorize_memory_grant};

/// Proof that one `(owner, role, action)` authorization passed the pipeline.
/// Carries the RESOLVED owner so check-site and use-site cannot diverge.
#[derive(Debug)]
pub struct MemoryPermit {
    owner: Owner,
    requested: Owner,
    role: Role,
    action: MemoryAction,
}

impl MemoryPermit {
    // PRIVATE: only `authorize_request` (same module) can mint.
    fn new(owner: Owner, requested: Owner, role: Role, action: MemoryAction) -> Self {
        Self {
            owner,
            requested,
            role,
            action,
        }
    }

    #[must_use]
    pub fn owner(&self) -> &Owner {
        &self.owner
    }

    #[must_use]
    pub fn requested(&self) -> &Owner {
        &self.requested
    }

    #[must_use]
    pub fn role(&self) -> Role {
        self.role
    }

    #[must_use]
    pub fn action(&self) -> MemoryAction {
        self.action
    }
}

impl Engine {
    /// The one Tier-2 gate. Resolves owner (identity for now), runs the existing
    /// decoupled primitives (role check AND owner-space grant, no coupling),
    /// mints the permit. The `(role, action)` pair stays explicit at the caller.
    #[expect(
        clippy::unused_self,
        reason = "later owner resolution will use engine state; Task 1 is identity-only"
    )]
    pub(in crate::engine) fn authorize_request(
        &self,
        authz: &AuthzContext,
        requested: &Owner,
        role: Role,
        action: MemoryAction,
    ) -> Result<MemoryPermit, ProtocolError> {
        let resolved: Owner = requested.clone();
        authorize(authz, &resolved, role)?;
        authorize_memory_grant(authz, &resolved, action)?;
        Ok(MemoryPermit::new(resolved, requested.clone(), role, action))
    }
}

#[cfg(test)]
mod tests {
    use crate::authz::{AuthPath, AuthzContext, MemoryAction, Role};
    use crate::error::ErrorCode;
    use crate::{FlavorRegistry, Owner, Principal, UserId};

    use super::Engine;

    fn engine() -> Engine {
        Engine::new(FlavorRegistry::new().freeze())
    }

    fn owner() -> Owner {
        Principal::User(UserId::new(uuid::Uuid::now_v7()))
    }

    #[test]
    fn single_owner_context_mints_matching_permit() {
        let engine = engine();
        let owner = owner();
        let authz = AuthzContext::single_owner(&owner, AuthPath::System);

        let permit = engine
            .authorize_request(&authz, &owner, Role::GraphRead, MemoryAction::Read)
            .expect("single-owner context should authorize");

        assert_eq!(permit.owner(), &owner);
        assert_eq!(permit.requested(), &owner);
        assert_eq!(permit.role(), Role::GraphRead);
        assert_eq!(permit.action(), MemoryAction::Read);
    }

    #[test]
    fn denied_context_returns_forbidden() {
        let engine = engine();
        let owner = owner();
        let authz = AuthzContext::denied(&owner);

        let err = engine
            .authorize_request(&authz, &owner, Role::GraphRead, MemoryAction::Read)
            .expect_err("denied context should reject authorization");

        assert_eq!(err.code, ErrorCode::Forbidden);
    }
}
