use crate::access::{EntityId, Relation, world};
use crate::authz::{AuthPath, AuthzContext, AuthzInput, AuthzOperation, AuthzOutcome};
use crate::error::ProtocolError;
use crate::storage::StorageError;
use crate::{Owner, OwnerRef};

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
    /// Caller operates within the owner-space — the sole surviving permit mode.
    /// The single-owner read verbs (`change_history` / `read_mcp_call_history`
    /// via `authorize_request`) mint it; the entry-scoped and public-read modes
    /// of the retired grant model are gone with their gate, replaced by
    /// `authorize_entry_read` / source-owned reads.
    OwnerScoped,
}

/// Proof that the resolved owner passed a write gate for `relation`. Sealed:
/// only this module's authorization gates can mint it.
#[derive(Debug)]
pub struct WritePermit {
    owner: OwnerRef,
    relation: Relation,
}

impl WritePermit {
    #[must_use]
    pub fn owner(&self) -> &OwnerRef {
        &self.owner
    }

    #[must_use]
    pub fn relation(&self) -> Relation {
        self.relation
    }
}

impl From<WritePermit> for MemoryPermit {
    fn from(permit: WritePermit) -> Self {
        Self::owner_scoped(permit.owner, permit.owner, permit.relation)
    }
}

/// Proof that one entry passed the read-scope predicate. Sealed: only this
/// module's authorization gates can mint it.
#[derive(Debug)]
pub struct EntryReadPermit {
    owner: OwnerRef,
}

impl EntryReadPermit {
    #[must_use]
    pub fn owner(&self) -> &OwnerRef {
        &self.owner
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AccessBasis {
    ActingAsOwner,
}

impl MemoryPermit {
    fn owner_scoped(owner: Owner, requested: Owner, relation: Relation) -> Self {
        Self {
            mode: PermitMode::OwnerScoped,
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
}

impl Engine {
    /// Targeted write/config gate over the resolved access sets.
    pub(in crate::engine) async fn authorize_write(
        &self,
        authz: &AuthzContext,
        owner: &OwnerRef,
        required: Relation,
    ) -> Result<WritePermit, ProtocolError> {
        if authz.auth_path() == AuthPath::Denied {
            let input = AuthzInput {
                authz,
                requested: owner,
                resolved: owner,
                relation: required,
                operation: AuthzOperation::Relation { relation: required },
            };
            self.registry
                .run_authorization_observers(&input, AuthzOutcome::DeniedResolution);
            return Err(ProtocolError::forbidden(
                "denied context authorizes nothing",
            ));
        }

        let resolved = match self.registry.resolve_owner(authz, owner) {
            Ok(owner) => owner,
            Err(err) => {
                let input = AuthzInput {
                    authz,
                    requested: owner,
                    resolved: owner,
                    relation: required,
                    operation: AuthzOperation::Relation { relation: required },
                };
                self.registry
                    .run_authorization_observers(&input, AuthzOutcome::DeniedResolution);
                return Err(err);
            }
        };

        let input = AuthzInput {
            authz,
            requested: owner,
            resolved: &resolved,
            relation: required,
            operation: AuthzOperation::Relation { relation: required },
        };

        if resolved == world() {
            self.registry
                .run_authorization_observers(&input, AuthzOutcome::DeniedGrant);
            return Err(ProtocolError::forbidden(
                "World is read-only and never a write owner",
            ));
        }

        let access = self.resolve_access(authz).await?;
        if !access.can_write(&resolved, required) {
            self.registry
                .run_authorization_observers(&input, AuthzOutcome::DeniedGrant);
            return Err(ProtocolError::forbidden(required.denied_message()));
        }

        if let Err(err) = self.registry.run_authorization_vetoes(&input) {
            self.registry
                .run_authorization_observers(&input, AuthzOutcome::DeniedVeto);
            return Err(err);
        }

        self.registry
            .run_authorization_observers(&input, AuthzOutcome::Allowed);
        Ok(WritePermit {
            owner: resolved,
            relation: required,
        })
    }

    /// Read gate returning the resolved owner set visible to this context.
    pub(in crate::engine) async fn authorize_read(
        &self,
        authz: &AuthzContext,
    ) -> Result<Vec<OwnerRef>, ProtocolError> {
        let access = self.resolve_access(authz).await?;
        let read = access.read_owners().to_vec();
        let principal = authz.principal();
        let input = AuthzInput {
            authz,
            requested: &principal,
            resolved: &principal,
            relation: Relation::Viewer,
            operation: AuthzOperation::Relation {
                relation: Relation::Viewer,
            },
        };

        if read.is_empty() {
            self.registry
                .run_authorization_observers(&input, AuthzOutcome::DeniedResolution);
            return Err(ProtocolError::forbidden(
                "denied context authorizes nothing",
            ));
        }

        self.registry
            .run_authorization_observers(&input, AuthzOutcome::Allowed);
        Ok(read)
    }

    /// Single-entry read gate. Existence is not disclosed to non-readers.
    pub(in crate::engine) async fn authorize_entry_read(
        &self,
        authz: &AuthzContext,
        entity: EntityId,
    ) -> Result<EntryReadPermit, ProtocolError> {
        let read = self.authorize_read(authz).await?;
        let home = self
            .storage()
            .pipeline
            .owner_access_read
            .home_owner(entity)
            .await
            .map_err(|err| storage_error("home_owner", &err))?
            .ok_or_else(|| ProtocolError::forbidden("entry not found"))?;

        let readable = self
            .storage()
            .pipeline
            .owner_access_read
            .visible_to_any(entity, &read)
            .await
            .map_err(|err| storage_error("visible_to_any", &err))?;
        if !readable {
            return Err(ProtocolError::forbidden("entry not found"));
        }

        Ok(EntryReadPermit { owner: home })
    }

    /// The one Tier-2 owner/space-scoped gate. Async because relation resolution
    /// reads persisted grants (skipped for Unrestricted). `denied` contexts are
    /// rejected before any resolution (replaces the old `RoleSet::none` gate).
    pub(in crate::engine) async fn authorize_request(
        &self,
        authz: &AuthzContext,
        requested: &Owner,
        relation: Relation,
    ) -> Result<MemoryPermit, ProtocolError> {
        if authz.auth_path() == AuthPath::Denied {
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
        match self.gate_and_veto(authz, &resolved, relation, &input).await {
            Ok(_basis) => {}
            Err((err, outcome)) => {
                self.registry.run_authorization_observers(&input, outcome);
                return Err(err);
            }
        }
        let permit = MemoryPermit::owner_scoped(resolved, *requested, relation);
        self.registry
            .run_authorization_observers(&input, AuthzOutcome::Allowed);
        Ok(permit)
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

    /// Relation gate over the server-resolved owner access sets.
    async fn resolve_relation(
        &self,
        authz: &AuthzContext,
        owner: &Owner,
        relation: Relation,
    ) -> Result<AccessBasis, ProtocolError> {
        let access = self.resolve_access(authz).await?;
        let allowed = if relation == Relation::Viewer {
            access.can_read(owner)
        } else {
            access.can_write(owner, relation)
        };
        if allowed {
            Ok(AccessBasis::ActingAsOwner)
        } else {
            Err(ProtocolError::forbidden(relation.denied_message()))
        }
    }
}

#[allow(dead_code)]
fn storage_error(context: &str, err: &StorageError) -> ProtocolError {
    ProtocolError::internal(format!("{context}: {err}"))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use crate::access::{AccessScope, EntityId, Relation, world};
    use crate::authz::{
        AuthPath, AuthorizationHook, AuthzContext, AuthzInput, AuthzOperation, AuthzOutcome,
        AuthzVeto, OwnerResolver, ToolScope,
    };
    use crate::error::ErrorCode;
    use crate::error::ProtocolError;
    use crate::{FlavorRegistry, GroupId, MemoryId, Owner, OwnerRef, UserId};

    use super::super::access_sets::tests::MembershipStorage;
    use super::{AccessBasis, Engine, PermitMode};

    type ResolvedAuthz = AuthzContext;

    fn engine() -> Engine {
        Engine::new(FlavorRegistry::new().freeze())
    }

    fn engine_from_registry(registry: FlavorRegistry) -> Engine {
        Engine::new(registry.freeze())
    }

    fn engine_with_ports(storage: MembershipStorage) -> Engine {
        Engine::compose(storage.storage_ports(), |_| {})
    }

    fn engine_from_registry_and_storage(
        registry: FlavorRegistry,
        storage: MembershipStorage,
    ) -> Engine {
        Engine::new(registry.freeze()).with_storage_ports(storage.storage_ports())
    }

    fn owner() -> Owner {
        OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()))
    }

    fn group_owner() -> Owner {
        OwnerRef::Group(GroupId::new(uuid::Uuid::now_v7()))
    }

    fn storage(member: OwnerRef, group: GroupId) -> MembershipStorage {
        storage_with_relation(member, group, Relation::Viewer)
    }

    fn storage_with_relation(
        member: OwnerRef,
        group: GroupId,
        membership_relation: Relation,
    ) -> MembershipStorage {
        MembershipStorage {
            member,
            group,
            membership_relation,
            home_owner: None,
            entity_readable: false,
            memory_kind: None,
        }
    }

    fn storage_with_entity(
        member: OwnerRef,
        group: GroupId,
        home_owner: Option<OwnerRef>,
        entity_readable: bool,
    ) -> MembershipStorage {
        MembershipStorage {
            member,
            group,
            membership_relation: Relation::Viewer,
            home_owner,
            entity_readable,
            memory_kind: None,
        }
    }

    fn granted_context(owner: &Owner) -> ResolvedAuthz {
        AuthzContext::scoped_access(
            *owner,
            [*owner],
            ToolScope::All,
            AccessScope::Granted,
            AuthPath::HostBearer,
        )
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
            Ok(self.resolved)
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
                input.authz.auth_path(),
                AuthPath::System | AuthPath::HostBearer
            ));
            assert_eq!(input.relation, Relation::Viewer);
            assert_eq!(
                input.operation,
                AuthzOperation::Relation {
                    relation: Relation::Viewer
                }
            );
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
        assert!(matches!(permit.mode(), PermitMode::OwnerScoped));
    }

    #[tokio::test]
    async fn authorize_write_allows_self_editor() {
        let p = owner();
        let g1 = GroupId::new(uuid::Uuid::now_v7());
        let engine = engine_with_ports(storage(p, g1));
        let authz = granted_context(&p);

        let permit = engine
            .authorize_write(&authz, &p, Relation::Editor)
            .await
            .expect("granted self editor should authorize");

        assert_eq!(permit.owner(), &p);
        assert_eq!(permit.relation(), Relation::Editor);
    }

    #[tokio::test]
    async fn authorize_write_denies_viewer_for_editor() {
        let p = owner();
        let g1 = GroupId::new(uuid::Uuid::now_v7());
        let g1_owner = OwnerRef::Group(g1);
        let engine = engine_with_ports(storage(p, g1));
        let authz = granted_context(&p);

        let err = engine
            .authorize_write(&authz, &g1_owner, Relation::Editor)
            .await
            .expect_err("viewer membership should not authorize editor writes");

        assert_eq!(err.code, ErrorCode::Forbidden);
    }

    #[tokio::test]
    async fn authorize_write_allows_editor_member_for_group() {
        let p = owner();
        let g1 = GroupId::new(uuid::Uuid::now_v7());
        let g1_owner = OwnerRef::Group(g1);
        let engine = engine_with_ports(storage_with_relation(p, g1, Relation::Editor));
        let authz = granted_context(&p);

        let permit = engine
            .authorize_write(&authz, &g1_owner, Relation::Editor)
            .await
            .expect("editor membership should authorize editor writes");

        assert_eq!(permit.owner(), &g1_owner);
        assert_eq!(permit.relation(), Relation::Editor);
    }

    #[tokio::test]
    async fn authorize_write_veto_denies() {
        let p = owner();
        let g1 = GroupId::new(uuid::Uuid::now_v7());
        let mut registry = FlavorRegistry::new();
        registry.add_authorization_hook(Arc::new(VetoHook));
        let engine = engine_from_registry_and_storage(registry, storage(p, g1));
        let authz = granted_context(&p);

        let err = engine
            .authorize_write(&authz, &p, Relation::Editor)
            .await
            .expect_err("veto should deny otherwise-allowed write");

        assert_eq!(err.code, ErrorCode::Forbidden);
        assert_eq!(err.message, "test veto");
    }

    #[tokio::test]
    async fn authorize_write_denied_context_forbidden() {
        let p = owner();
        let g1 = GroupId::new(uuid::Uuid::now_v7());
        let engine = engine_with_ports(storage(p, g1));
        let authz = AuthzContext::denied_for_owner(&p);

        let err = engine
            .authorize_write(&authz, &p, Relation::Editor)
            .await
            .expect_err("denied context should reject write authorization");

        assert_eq!(err.code, ErrorCode::Forbidden);
    }

    #[tokio::test]
    async fn authorize_write_denies_world_for_every_write_relation() {
        let p = owner();
        let g1 = GroupId::new(uuid::Uuid::now_v7());
        let engine = engine_with_ports(storage(p, g1));
        let authz = AuthzContext::scoped_access(
            p,
            [p, world()],
            ToolScope::All,
            AccessScope::Unrestricted,
            AuthPath::HostBearer,
        );

        for relation in [Relation::Admin, Relation::Editor, Relation::Ingest] {
            let err = engine
                .authorize_write(&authz, &world(), relation)
                .await
                .expect_err("World must never be writable");

            assert_eq!(err.code, ErrorCode::Forbidden);
            assert_eq!(err.message, "World is read-only and never a write owner");
        }
    }

    #[tokio::test]
    async fn authorize_read_returns_world_and_groups() {
        let p = owner();
        let g1 = GroupId::new(uuid::Uuid::now_v7());
        let g1_owner = OwnerRef::Group(g1);
        let engine = engine_with_ports(storage(p, g1));
        let authz = granted_context(&p);

        let read = engine
            .authorize_read(&authz)
            .await
            .expect("granted context should resolve read owners");

        assert!(read.contains(&p));
        assert!(read.contains(&g1_owner));
        assert!(read.contains(&world()));
    }

    #[tokio::test]
    async fn authorize_read_denied_forbidden() {
        let p = owner();
        let g1 = GroupId::new(uuid::Uuid::now_v7());
        let engine = engine_with_ports(storage(p, g1));
        let authz = AuthzContext::denied_for_owner(&p);

        let err = engine
            .authorize_read(&authz)
            .await
            .expect_err("denied context should reject read authorization");

        assert_eq!(err.code, ErrorCode::Forbidden);
    }

    #[tokio::test]
    async fn authorize_entry_read_ok_when_readable() {
        let p = owner();
        let g1 = GroupId::new(uuid::Uuid::now_v7());
        let engine = engine_with_ports(storage_with_entity(p, g1, Some(p), true));
        let authz = granted_context(&p);

        let permit = engine
            .authorize_entry_read(
                &authz,
                EntityId::Memory(MemoryId::new(uuid::Uuid::now_v7())),
            )
            .await
            .expect("readable entity should authorize");

        assert_eq!(permit.owner(), &p);
    }

    #[tokio::test]
    async fn authorize_entry_read_absent_is_forbidden() {
        let p = owner();
        let g1 = GroupId::new(uuid::Uuid::now_v7());
        let engine = engine_with_ports(storage_with_entity(p, g1, None, true));
        let authz = granted_context(&p);

        let err = engine
            .authorize_entry_read(
                &authz,
                EntityId::Memory(MemoryId::new(uuid::Uuid::now_v7())),
            )
            .await
            .expect_err("absent entity should fail closed");

        assert_eq!(err.code, ErrorCode::Forbidden);
        assert_eq!(err.message, "entry not found");
    }

    #[tokio::test]
    async fn authorize_entry_read_unreadable_is_forbidden() {
        let p = owner();
        let other = owner();
        let g1 = GroupId::new(uuid::Uuid::now_v7());
        let engine = engine_with_ports(storage_with_entity(p, g1, Some(other), false));
        let authz = granted_context(&p);

        let err = engine
            .authorize_entry_read(
                &authz,
                EntityId::Memory(MemoryId::new(uuid::Uuid::now_v7())),
            )
            .await
            .expect_err("unreadable entity should fail closed");

        assert_eq!(err.code, ErrorCode::Forbidden);
        assert_eq!(err.message, "entry not found");
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
        let authz = AuthzContext::denied_for_owner(&owner);

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
        let authz = AuthzContext::denied_for_owner(&owner);

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
    async fn resolver_remap_still_gates_resolved_owner() {
        let requested = owner();
        let hidden = owner();
        let mut registry = FlavorRegistry::new();
        registry.set_owner_resolver(Arc::new(StaticResolver { resolved: hidden }));
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
