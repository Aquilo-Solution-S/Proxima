use crate::access::{AccessScope, Relation};
use crate::authz::{AuthPath, AuthzContext, AuthzInput, AuthzOperation, AuthzOutcome};
use crate::error::ProtocolError;
use crate::{MemoryId, Owner, PersonalityInstanceId, Principal};

use super::Engine;

/// Proof that one `(owner, relation)` authorization passed the pipeline. Carries
/// the RESOLVED owner so check-site and use-site cannot diverge. Sealed: only
/// this module's authorization gates can mint it.
#[derive(Debug)]
pub struct MemoryPermit {
    mode: PermitMode,
    owner: Owner,
    requested: Owner,
    relation: Relation,
}

#[derive(Debug, Clone)]
pub enum PermitMode {
    /// Caller operates within the owner-space.
    OwnerScoped {
        subject_personality: Option<PersonalityInstanceId>,
    },
    /// Cross-principal accessor reaching one entry via an entry-level grant.
    EntryScoped {
        resource: MemoryId,
        subject_personality: PersonalityInstanceId,
    },
    /// World-readable published entry. Resource-only.
    PublicRead { resource: MemoryId },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AccessBasis {
    ActingAsOwner,
}

impl MemoryPermit {
    fn owner_scoped(
        owner: Owner,
        requested: Owner,
        relation: Relation,
        subject_personality: Option<PersonalityInstanceId>,
    ) -> Self {
        Self {
            mode: PermitMode::OwnerScoped {
                subject_personality,
            },
            owner,
            requested,
            relation,
        }
    }

    fn entry(mode: PermitMode, owner: Owner, requested: Owner, relation: Relation) -> Self {
        Self {
            mode,
            owner,
            requested,
            relation,
        }
    }

    #[must_use]
    pub fn mode(&self) -> &PermitMode {
        &self.mode
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
    pub fn relation(&self) -> Relation {
        self.relation
    }

    #[must_use]
    pub fn subject_personality(&self) -> Option<PersonalityInstanceId> {
        match &self.mode {
            PermitMode::OwnerScoped {
                subject_personality,
            } => *subject_personality,
            PermitMode::EntryScoped {
                subject_personality,
                ..
            } => Some(*subject_personality),
            PermitMode::PublicRead { .. } => None,
        }
    }
}

impl Engine {
    /// The one Tier-2 owner/space-scoped gate. Async because relation resolution
    /// reads persisted grants (skipped for Unrestricted). `denied` contexts are
    /// rejected before any resolution (replaces the old `RoleSet::none` gate).
    pub(in crate::engine) async fn authorize_request(
        &self,
        authz: &AuthzContext,
        requested: &Owner,
        relation: Relation,
    ) -> Result<MemoryPermit, ProtocolError> {
        if authz.auth_path == AuthPath::Denied {
            let input = AuthzInput {
                authz,
                requested,
                resolved: requested,
                relation,
                operation: AuthzOperation::Relation { relation },
            };
            self.registry
                .run_authorization_observers(&input, AuthzOutcome::DeniedResolution);
            return Err(ProtocolError::forbidden(
                "denied context authorizes nothing",
            ));
        }
        let resolved = match self.registry.resolve_owner(authz, requested) {
            Ok(owner) => owner,
            Err(err) => {
                let input = AuthzInput {
                    authz,
                    requested,
                    resolved: requested,
                    relation,
                    operation: AuthzOperation::Relation { relation },
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
            relation,
            operation: AuthzOperation::Relation { relation },
        };
        let basis = match self.gate_and_veto(authz, &resolved, relation, &input).await {
            Ok(basis) => basis,
            Err((err, outcome)) => {
                self.registry.run_authorization_observers(&input, outcome);
                return Err(err);
            }
        };
        let subject_personality = match basis {
            AccessBasis::ActingAsOwner => None,
        };
        let permit = MemoryPermit::owner_scoped(
            resolved.clone(),
            requested.clone(),
            relation,
            subject_personality,
        );
        self.registry
            .run_authorization_observers(&input, AuthzOutcome::Allowed);
        Ok(permit)
    }

    /// Resource-scoped single-entry gate. Entry owner is resolved inside
    /// storage, so callers cannot select the owner-space used for the read.
    // TEMP: grant resolution was gutted in the vocab swap; the .await returns in
    // Task 5.3/Phase 4 when this is replaced by `authorize_entry_read`. Remove then.
    #[allow(clippy::unused_async)]
    pub(in crate::engine) async fn authorize_entry_request(
        &self,
        authz: &AuthzContext,
        memory_id: MemoryId,
        relation: Relation,
    ) -> Result<MemoryPermit, ProtocolError> {
        if authz.auth_path == AuthPath::Denied {
            self.observe_unresolved_entry(authz, memory_id, relation);
            return Err(ProtocolError::forbidden(
                "denied context authorizes nothing",
            ));
        }
        let owner = authz.identity.principal.clone();
        let input = AuthzInput {
            authz,
            requested: &owner,
            resolved: &owner,
            relation,
            operation: AuthzOperation::Relation { relation },
        };

        let principal = &authz.identity.principal;
        let identity_owner = matches!(owner, Principal::User(_))
            && principal == &owner
            && authz.identity.can_access_principal(&owner);
        let unrestricted = authz.capabilities.access == AccessScope::Unrestricted
            && authz.identity.can_access_principal(&owner);
        if identity_owner || unrestricted {
            self.veto_and_observe(&input)?;
            return Ok(MemoryPermit::entry(
                PermitMode::OwnerScoped {
                    subject_personality: None,
                },
                owner.clone(),
                owner,
                relation,
            ));
        }

        let _ = (memory_id, principal);
        self.registry
            .run_authorization_observers(&input, AuthzOutcome::DeniedGrant);
        Err(ProtocolError::forbidden(relation.denied_message()))
    }

    async fn gate_and_veto(
        &self,
        authz: &AuthzContext,
        owner: &Owner,
        relation: Relation,
        input: &AuthzInput<'_>,
    ) -> Result<AccessBasis, (ProtocolError, AuthzOutcome)> {
        let basis = self
            .resolve_relation(authz, owner, relation)
            .await
            .map_err(|err| (err, AuthzOutcome::DeniedGrant))?;
        self.registry
            .run_authorization_vetoes(input)
            .map_err(|err| (err, AuthzOutcome::DeniedVeto))?;
        Ok(basis)
    }

    fn veto_and_observe(&self, input: &AuthzInput<'_>) -> Result<(), ProtocolError> {
        let (result, outcome) = match self.registry.run_authorization_vetoes(input) {
            Ok(()) => (Ok(()), AuthzOutcome::Allowed),
            Err(err) => (Err(err), AuthzOutcome::DeniedVeto),
        };
        self.registry.run_authorization_observers(input, outcome);
        result
    }

    fn observe_unresolved_entry(
        &self,
        authz: &AuthzContext,
        _memory_id: MemoryId,
        relation: Relation,
    ) {
        let unresolved = authz.identity.principal.clone();
        let input = AuthzInput {
            authz,
            requested: &unresolved,
            resolved: &unresolved,
            relation,
            operation: AuthzOperation::Relation { relation },
        };
        self.registry
            .run_authorization_observers(&input, AuthzOutcome::DeniedResolution);
    }

    /// Steps 0/2/3 of the spec algorithm. `can_access` gates ONLY identity-owner +
    /// unrestricted; persisted space bindings (incl. owner rows + group member
    /// inheritance) are NOT can_access-gated — that is how cross-principal access
    /// works. Resolution is short-circuited for Unrestricted/identity before any DB read.
    // TEMP: grant-DB resolution removed in the vocab swap; deleted in Phase 4 with the
    // old gate (replaced by AccessSets::can_write). Remove the allow then.
    #[allow(clippy::unused_async)]
    async fn resolve_relation(
        &self,
        authz: &AuthzContext,
        owner: &Owner,
        relation: Relation,
    ) -> Result<AccessBasis, ProtocolError> {
        let principal = &authz.identity.principal;
        let identity_owner = matches!(owner, Principal::User(_))
            && principal == owner
            && authz.identity.can_access_principal(owner);
        let unrestricted = authz.capabilities.access == AccessScope::Unrestricted
            && authz.identity.can_access_principal(owner);
        if identity_owner || unrestricted {
            return Ok(AccessBasis::ActingAsOwner);
        }
        let _ = principal;
        Err(ProtocolError::forbidden(relation.denied_message()))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::{Arc, Mutex};

    use crate::access::{AccessScope, Relation};
    use crate::authz::{
        AuthPath, AuthorizationHook, AuthzContext, AuthzInput, AuthzOperation, AuthzOutcome,
        AuthzVeto, CapabilitySet, Identity, OwnerResolver, ToolScope,
    };
    use crate::error::ErrorCode;
    use crate::error::ProtocolError;
    use crate::{FlavorRegistry, GroupId, MemoryId, Owner, Principal, UserId};

    use super::{AccessBasis, Engine, PermitMode};

    fn engine() -> Engine {
        Engine::new(FlavorRegistry::new().freeze())
    }

    fn engine_from_registry(registry: FlavorRegistry) -> Engine {
        Engine::new(registry.freeze())
    }

    fn owner() -> Owner {
        Principal::User(UserId::new(uuid::Uuid::now_v7()))
    }

    fn group_owner() -> Owner {
        Principal::Group(GroupId::new(uuid::Uuid::now_v7()))
    }

    fn granted_context(owner: &Owner) -> AuthzContext {
        let mut accessible_principals = HashSet::new();
        accessible_principals.insert(owner.clone());
        AuthzContext {
            identity: Identity {
                principal: owner.clone(),
                accessible_principals,
                expires_at: None,
                auth_epoch: 0,
            },
            capabilities: CapabilitySet {
                tool_scope: ToolScope::All,
                access: AccessScope::Granted,
            },
            auth_path: AuthPath::HostBearer,
        }
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
                AuthPath::System | AuthPath::HostBearer
            ));
            assert_eq!(input.relation, Relation::Viewer);
            assert_eq!(input.operation, AuthzOperation::Relation { relation: Relation::Viewer });
            self.outcomes.lock().expect("recorder lock").push(outcome);
        }
    }

    #[derive(Debug)]
    struct RecordingAnyHook {
        outcomes: Arc<Mutex<Vec<AuthzOutcome>>>,
    }

    impl AuthorizationHook for RecordingAnyHook {
        fn observe(&self, _input: &AuthzInput<'_>, outcome: AuthzOutcome) {
            self.outcomes.lock().expect("recorder lock").push(outcome);
        }
    }

    #[tokio::test]
    async fn single_owner_context_mints_matching_permit() {
        let engine = engine();
        let owner = owner();
        let authz = AuthzContext::single_owner(&owner, AuthPath::System);

        let permit = engine
            .authorize_request(&authz, &owner, Relation::Viewer)
            .await
            .expect("single-owner context should authorize");

        assert_eq!(permit.owner(), &owner);
        assert_eq!(permit.requested(), &owner);
        assert_eq!(permit.relation(), Relation::Viewer);
        assert!(matches!(
            permit.mode(),
            PermitMode::OwnerScoped {
                subject_personality: None
            }
        ));
        assert_eq!(permit.subject_personality(), None);
    }

    #[tokio::test]
    async fn resolve_relation_reports_acting_as_owner_for_single_owner_context() {
        let engine = engine();
        let owner = owner();
        let authz = AuthzContext::single_owner(&owner, AuthPath::System);

        let basis = engine
            .resolve_relation(&authz, &owner, Relation::Viewer)
            .await
            .expect("single-owner context should authorize");

        assert_eq!(basis, AccessBasis::ActingAsOwner);
    }

    #[tokio::test]
    async fn denied_context_returns_forbidden() {
        let engine = engine();
        let owner = owner();
        let authz = AuthzContext::denied(&owner);

        let err = engine
            .authorize_request(&authz, &owner, Relation::Viewer)
            .await
            .expect_err("denied context should reject authorization");

        assert_eq!(err.code, ErrorCode::Forbidden);
    }

    #[tokio::test]
    async fn denied_context_notifies_observers() {
        let owner = owner();
        let outcomes = Arc::new(Mutex::new(Vec::new()));
        let mut registry = FlavorRegistry::new();
        registry.add_authorization_hook(Arc::new(RecordingAnyHook {
            outcomes: outcomes.clone(),
        }));
        let engine = engine_from_registry(registry);
        let authz = AuthzContext::denied(&owner);

        let err = engine
            .authorize_request(&authz, &owner, Relation::Viewer)
            .await
            .expect_err("denied context should reject authorization");

        assert_eq!(err.code, ErrorCode::Forbidden);
        assert_eq!(
            *outcomes.lock().expect("recorder lock"),
            vec![AuthzOutcome::DeniedResolution],
        );
    }

    #[tokio::test]
    async fn entry_request_denies_denied_context() {
        let engine = engine();
        let owner = owner();
        let authz = AuthzContext::denied(&owner);

        let err = engine
            .authorize_entry_request(
                &authz,
                MemoryId::new(uuid::Uuid::now_v7()),
                Relation::Viewer,
            )
            .await
            .expect_err("denied context should reject entry authorization");

        assert_eq!(err.code, ErrorCode::Forbidden);
    }

    #[tokio::test]
    async fn denied_entry_context_notifies_observers() {
        let owner = owner();
        let outcomes = Arc::new(Mutex::new(Vec::new()));
        let mut registry = FlavorRegistry::new();
        registry.add_authorization_hook(Arc::new(RecordingAnyHook {
            outcomes: outcomes.clone(),
        }));
        let engine = engine_from_registry(registry);
        let authz = AuthzContext::denied(&owner);

        let err = engine
            .authorize_entry_request(
                &authz,
                MemoryId::new(uuid::Uuid::now_v7()),
                Relation::Viewer,
            )
            .await
            .expect_err("denied context should reject entry authorization");

        assert_eq!(err.code, ErrorCode::Forbidden);
        assert_eq!(
            *outcomes.lock().expect("recorder lock"),
            vec![AuthzOutcome::DeniedResolution],
        );
    }

    #[tokio::test]
    async fn entry_request_absent_entry_returns_forbidden() {
        let engine = engine();
        let owner = owner();
        let authz = AuthzContext::single_owner(&owner, AuthPath::System);

        let err = engine
            .authorize_entry_request(
                &authz,
                MemoryId::new(uuid::Uuid::now_v7()),
                Relation::Viewer,
            )
            .await
            .expect_err("absent entry should fail closed");

        assert_eq!(err.code, ErrorCode::Forbidden);
        assert_eq!(err.message, "entry not found");
    }

    #[tokio::test]
    async fn resolver_remap_still_gates_resolved_owner() {
        let requested = owner();
        let hidden = owner();
        let mut registry = FlavorRegistry::new();
        registry.set_owner_resolver(Arc::new(StaticResolver {
            resolved: hidden.clone(),
        }));
        let engine = engine_from_registry(registry);
        let authz = AuthzContext::single_owner(&requested, AuthPath::System);

        let err = engine
            .authorize_request(&authz, &requested, Relation::Viewer)
            .await
            .expect_err("resolved hidden owner should be denied");

        assert_eq!(err.code, ErrorCode::Forbidden);
    }

    #[tokio::test]
    async fn veto_hook_denies_otherwise_allowed_request() {
        let owner = owner();
        let mut registry = FlavorRegistry::new();
        registry.add_authorization_hook(Arc::new(VetoHook));
        let engine = engine_from_registry(registry);
        let authz = AuthzContext::single_owner(&owner, AuthPath::System);

        let err = engine
            .authorize_request(&authz, &owner, Relation::Viewer)
            .await
            .expect_err("veto should deny otherwise-allowed request");

        assert_eq!(err.code, ErrorCode::Forbidden);
        assert_eq!(err.message, "test veto");
    }

    #[tokio::test]
    async fn observer_records_allowed_and_denied_outcomes() {
        let owner = owner();
        let denied_owner = group_owner();
        let outcomes = Arc::new(Mutex::new(Vec::new()));
        let mut registry = FlavorRegistry::new();
        registry.add_authorization_hook(Arc::new(RecordingHook {
            outcomes: outcomes.clone(),
        }));
        let engine = engine_from_registry(registry);
        let allowed = AuthzContext::single_owner(&owner, AuthPath::System);
        let denied = granted_context(&owner);

        engine
            .authorize_request(&allowed, &owner, Relation::Viewer)
            .await
            .expect("allowed request should pass");
        let err = engine
            .authorize_request(&denied, &denied_owner, Relation::Viewer)
            .await
            .expect_err("denied context should reject authorization");

        assert_eq!(err.code, ErrorCode::Forbidden);
        assert_eq!(
            *outcomes.lock().expect("recorder lock"),
            vec![AuthzOutcome::Allowed, AuthzOutcome::DeniedGrant],
        );
    }
}
