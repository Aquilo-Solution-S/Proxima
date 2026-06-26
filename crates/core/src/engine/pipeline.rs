//! The single Tier-2 authorization chokepoint. `MemoryPermit` is sealed: its
//! only constructor is `Engine::authorize_request` in this module, so a verb
//! body that requires a permit cannot run without one and cannot forge one.

use crate::Owner;
use crate::authz::{AuthzContext, AuthzInput, AuthzOutcome, MemoryAction, Role};
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
    /// The one Tier-2 gate. Resolves owner, runs the existing decoupled
    /// primitives (role check AND owner-space grant, no coupling), runs
    /// deny-only veto hooks, then mints the permit. The `(role, action)` pair
    /// stays explicit at the caller.
    pub(in crate::engine) fn authorize_request(
        &self,
        authz: &AuthzContext,
        requested: &Owner,
        role: Role,
        action: MemoryAction,
    ) -> Result<MemoryPermit, ProtocolError> {
        let resolved = match self.registry.resolve_owner(authz, requested) {
            Ok(owner) => owner,
            Err(err) => {
                let input = AuthzInput {
                    authz,
                    requested,
                    resolved: requested,
                    role,
                    action,
                };
                self.registry
                    .run_authorization_observers(&input, AuthzOutcome::DeniedResolution);
                return Err(err);
            }
        };
        let input = AuthzInput {
            authz,
            requested,
            resolved: &resolved,
            role,
            action,
        };
        let (result, outcome) = match self.gate_and_veto(authz, &resolved, role, action, &input) {
            Ok(()) => (Ok(()), AuthzOutcome::Allowed),
            Err((err, outcome)) => (Err(err), outcome),
        };
        self.registry.run_authorization_observers(&input, outcome);
        result?;
        Ok(MemoryPermit::new(resolved, requested.clone(), role, action))
    }

    fn gate_and_veto(
        &self,
        authz: &AuthzContext,
        owner: &Owner,
        role: Role,
        action: MemoryAction,
        input: &AuthzInput<'_>,
    ) -> Result<(), (ProtocolError, AuthzOutcome)> {
        authorize(authz, owner, role).map_err(|err| (err, AuthzOutcome::DeniedRole))?;
        authorize_memory_grant(authz, owner, action)
            .map_err(|err| (err, AuthzOutcome::DeniedGrant))?;
        self.registry
            .run_authorization_vetoes(input)
            .map_err(|err| (err, AuthzOutcome::DeniedVeto))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use crate::authz::{
        AuthPath, AuthorizationHook, AuthzContext, AuthzInput, AuthzOutcome, AuthzVeto,
        MemoryAction, OwnerResolver, Role,
    };
    use crate::error::ErrorCode;
    use crate::error::ProtocolError;
    use crate::{FlavorRegistry, Owner, Principal, UserId};

    use super::Engine;

    fn engine() -> Engine {
        Engine::new(FlavorRegistry::new().freeze())
    }

    fn engine_from_registry(registry: FlavorRegistry) -> Engine {
        Engine::new(registry.freeze())
    }

    fn owner() -> Owner {
        Principal::User(UserId::new(uuid::Uuid::now_v7()))
    }

    #[derive(Debug)]
    struct StaticResolver {
        resolved: Owner,
    }

    impl OwnerResolver for StaticResolver {
        fn resolve(
            &self,
            _authz: &AuthzContext,
            _requested: &Owner,
        ) -> Result<Owner, ProtocolError> {
            Ok(self.resolved.clone())
        }
    }

    #[derive(Debug)]
    struct VetoHook;

    impl AuthorizationHook for VetoHook {
        fn veto(&self, input: &AuthzInput<'_>) -> Result<(), AuthzVeto> {
            assert_eq!(input.requested, input.resolved);
            Err(AuthzVeto("test veto".into()))
        }
    }

    #[derive(Debug)]
    struct RecordingHook {
        outcomes: Arc<Mutex<Vec<AuthzOutcome>>>,
    }

    impl AuthorizationHook for RecordingHook {
        fn observe(&self, input: &AuthzInput<'_>, outcome: AuthzOutcome) {
            assert!(matches!(
                input.authz.auth_path,
                AuthPath::System | AuthPath::Denied
            ));
            assert_eq!(input.role, Role::GraphRead);
            assert_eq!(input.action, MemoryAction::Read);
            self.outcomes.lock().expect("recorder lock").push(outcome);
        }
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

    #[test]
    fn resolver_remap_still_gates_resolved_owner() {
        let requested = owner();
        let hidden = owner();
        let mut registry = FlavorRegistry::new();
        registry.set_owner_resolver(Arc::new(StaticResolver {
            resolved: hidden.clone(),
        }));
        let engine = engine_from_registry(registry);
        let authz = AuthzContext::single_owner(&requested, AuthPath::System);

        let err = engine
            .authorize_request(&authz, &requested, Role::GraphRead, MemoryAction::Read)
            .expect_err("resolved hidden owner should be denied");

        assert_eq!(err.code, ErrorCode::Forbidden);
    }

    #[test]
    fn veto_hook_denies_otherwise_allowed_request() {
        let owner = owner();
        let mut registry = FlavorRegistry::new();
        registry.add_authorization_hook(Arc::new(VetoHook));
        let engine = engine_from_registry(registry);
        let authz = AuthzContext::single_owner(&owner, AuthPath::System);

        let err = engine
            .authorize_request(&authz, &owner, Role::GraphRead, MemoryAction::Read)
            .expect_err("veto should deny otherwise-allowed request");

        assert_eq!(err.code, ErrorCode::Forbidden);
        assert_eq!(err.message, "test veto");
    }

    #[test]
    fn observer_records_allowed_and_denied_outcomes() {
        let owner = owner();
        let outcomes = Arc::new(Mutex::new(Vec::new()));
        let mut registry = FlavorRegistry::new();
        registry.add_authorization_hook(Arc::new(RecordingHook {
            outcomes: outcomes.clone(),
        }));
        let engine = engine_from_registry(registry);
        let allowed = AuthzContext::single_owner(&owner, AuthPath::System);
        let denied = AuthzContext::denied(&owner);

        engine
            .authorize_request(&allowed, &owner, Role::GraphRead, MemoryAction::Read)
            .expect("allowed request should pass");
        let err = engine
            .authorize_request(&denied, &owner, Role::GraphRead, MemoryAction::Read)
            .expect_err("denied context should reject authorization");

        assert_eq!(err.code, ErrorCode::Forbidden);
        assert_eq!(
            *outcomes.lock().expect("recorder lock"),
            vec![AuthzOutcome::Allowed, AuthzOutcome::DeniedRole],
        );
    }
}
