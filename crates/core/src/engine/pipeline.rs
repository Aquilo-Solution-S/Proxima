use crate::access::{AccessKind, EntityId, Role};
use crate::authz::{
    AuthPath, AuthzContext, AuthzInput, AuthzOperation, AuthzOutcome, EngineAuthority,
};
use crate::error::ProtocolError;
use crate::storage::StorageError;
use crate::storage_ports::OwnerWritePermit;
use crate::{Owner, OwnerRef};

use super::Engine;

/// Proof that the resolved owner passed a write gate. Sealed: only this
/// module's authorization gates can mint it.
#[derive(Debug)]
pub struct WritePermit {
    owner_write: OwnerWritePermit,
}

impl WritePermit {
    pub(in crate::engine) fn authorize_transfer_to(&mut self, destination: OwnerRef) {
        self.owner_write.authorize_transfer_to(destination);
    }

    #[must_use]
    pub fn owner(&self) -> &OwnerRef {
        self.owner_write.owner()
    }

    #[must_use]
    pub const fn owner_write_permit(&self) -> &OwnerWritePermit {
        &self.owner_write
    }

    pub(in crate::engine) fn into_owner_write(self) -> OwnerWritePermit {
        self.owner_write
    }

    #[must_use]
    pub fn owner_scope(&self) -> Option<&crate::OwnerScope> {
        self.owner_write.owner_scope()
    }

    /// Test-only write permit. The gates in this module remain the
    /// production mint; see
    /// [`crate::verbs::fact_ingest::AuthorizedFactWrite::new_for_tests`].
    #[cfg(any(test, feature = "test-fixtures"))]
    #[must_use]
    pub fn for_tests(owner_write: OwnerWritePermit) -> Self {
        Self { owner_write }
    }

    #[cfg(test)]
    pub(crate) fn expire_delegated_write_for_test(&mut self) {
        self.owner_write.expire_delegated_for_test();
    }
}

/// The uniform answer [`Engine::authorize_entry_read`] gives for both a
/// nonexistent entity and one the caller may not see — existence is not
/// disclosed to non-readers. Read verbs that want to present the case as
/// a not-found (rather than a forbidden) match on this constant.
pub(in crate::engine) const ENTRY_NOT_FOUND_MESSAGE: &str = "entry not found";

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

impl Engine {
    /// Storage-tier owner-write gate for `kind`. A `System` context must
    /// come from [`AuthzContext::for_system`] with this engine's
    /// [`crate::SystemAuthority`].
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when `authz` cannot write `kind` on `owner`.
    #[allow(
        clippy::unused_async,
        reason = "keeps the owner-write gate on the async Engine authorization seam"
    )]
    pub async fn authorize_owner_write(
        &self,
        authz: &AuthzContext,
        owner: &OwnerRef,
        kind: AccessKind,
    ) -> Result<OwnerWritePermit, ProtocolError> {
        Ok(self
            .write_permit(authz, owner, AuthzOperation::Write { kind })?
            .owner_write)
    }

    /// Write gate: the rule for `kind` on `owner`.
    #[allow(
        clippy::unused_async,
        reason = "keeps the write gate on the async Engine authorization seam"
    )]
    pub(in crate::engine) async fn authorize_write<A>(
        &self,
        authority: &A,
        owner: &OwnerRef,
        kind: AccessKind,
    ) -> Result<WritePermit, ProtocolError>
    where
        A: EngineAuthority + ?Sized,
    {
        self.write_permit(authority, owner, AuthzOperation::Write { kind })
    }

    /// Write gate for rows whose kind only storage reads (forget,
    /// hydration): the permit carries the caller's write limit on `owner`,
    /// and storage answers `NotFound` for a row above it.
    #[allow(
        clippy::unused_async,
        reason = "keeps the write gate on the async Engine authorization seam"
    )]
    pub(in crate::engine) async fn authorize_write_limit<A>(
        &self,
        authority: &A,
        owner: &OwnerRef,
    ) -> Result<WritePermit, ProtocolError>
    where
        A: EngineAuthority + ?Sized,
    {
        let kind = self
            .operation_authority(authority)?
            .authz()
            .role_for_owner(owner)
            .and_then(|role| role.write_ceiling().top())
            .unwrap_or(AccessKind::Fact);
        self.write_permit(authority, owner, AuthzOperation::Write { kind })
    }

    /// The owner-admin gate ([`Role::administers`]): membership admin,
    /// transfer, erase, the graph overview.
    #[allow(
        clippy::unused_async,
        reason = "keeps the owner-admin gate on the async Engine authorization seam"
    )]
    pub(in crate::engine) async fn authorize_owner_admin<A>(
        &self,
        authority: &A,
        owner: &OwnerRef,
    ) -> Result<WritePermit, ProtocolError>
    where
        A: EngineAuthority + ?Sized,
    {
        self.write_permit(authority, owner, AuthzOperation::OwnerAdmin)
    }

    fn write_permit<A>(
        &self,
        authority: &A,
        owner: &OwnerRef,
        operation: AuthzOperation,
    ) -> Result<WritePermit, ProtocolError>
    where
        A: EngineAuthority + ?Sized,
    {
        let authority = self.operation_authority(authority)?;
        let authz = authority.authz();
        let kind = match operation {
            AuthzOperation::Write { kind } => kind,
            _ => AccessKind::Goal,
        };
        let resolved = self.gate(authz, owner, operation, authority.redeemed_phase())?;
        let owner_write = if authority.redeemed_phase() {
            let expires_at = authz.expires_at().ok_or_else(|| {
                ProtocolError::forbidden("delegated worker phase has no finite expiry")
            })?;
            OwnerWritePermit::new_delegated(
                resolved,
                kind,
                self.delegation_runtime_binding.clone(),
                expires_at,
                authz.owner_scope().cloned(),
            )
        } else {
            OwnerWritePermit::new(resolved, kind, authz.owner_scope().cloned())
        };
        Ok(WritePermit { owner_write })
    }

    /// Read gate for one owner: the rule for `kind`. Returns the resolved
    /// owner.
    #[allow(
        clippy::unused_async,
        reason = "keeps the read gate on the async Engine authorization seam"
    )]
    pub(in crate::engine) async fn authorize_owner_read<A>(
        &self,
        authority: &A,
        owner: &Owner,
        kind: AccessKind,
    ) -> Result<Owner, ProtocolError>
    where
        A: EngineAuthority + ?Sized,
    {
        let authority = self.operation_authority(authority)?;
        self.gate(
            authority.authz(),
            owner,
            AuthzOperation::Read { kind },
            authority.redeemed_phase(),
        )
    }

    /// The one owner gate: resolve `owner`, apply the rule for `operation`,
    /// run the vetoes, and report the outcome to the observers.
    fn gate(
        &self,
        authz: &AuthzContext,
        owner: &OwnerRef,
        operation: AuthzOperation,
        redeemed_phase: bool,
    ) -> Result<OwnerRef, ProtocolError> {
        if authz.auth_path() == AuthPath::Delegated && !redeemed_phase {
            return Err(ProtocolError::forbidden(
                "raw delegated authorization contexts are not Engine authority",
            ));
        }
        let refuse = |resolved: &OwnerRef, outcome, err| {
            let input = AuthzInput {
                authz,
                requested: owner,
                resolved,
                operation: operation.clone(),
            };
            self.registry.run_authorization_observers(&input, outcome);
            Err(err)
        };
        let writes = !matches!(operation, AuthzOperation::Read { .. });
        if writes
            && authz.auth_path() == AuthPath::System
            && !authz.carries_system_authority(&self.system_authority_binding)
        {
            return refuse(
                owner,
                AuthzOutcome::DeniedGrant,
                ProtocolError::forbidden(
                    "System write authority requires a context from AuthzContext::for_system",
                ),
            );
        }
        if authz.auth_path() == AuthPath::Denied {
            return refuse(
                owner,
                AuthzOutcome::DeniedResolution,
                ProtocolError::forbidden("denied context authorizes nothing"),
            );
        }
        let resolved = match self.registry.resolve_owner(authz, owner) {
            Ok(resolved) => resolved,
            Err(err) => return refuse(owner, AuthzOutcome::DeniedResolution, err),
        };
        if !allows(authz, &resolved, &operation) {
            return refuse(&resolved, AuthzOutcome::DeniedGrant, denied(&operation));
        }
        let input = AuthzInput {
            authz,
            requested: owner,
            resolved: &resolved,
            operation,
        };
        if let Err(err) = self.registry.run_authorization_vetoes(&input) {
            self.registry
                .run_authorization_observers(&input, AuthzOutcome::DeniedVeto);
            return Err(err);
        }
        self.registry
            .run_authorization_observers(&input, AuthzOutcome::Allowed);
        Ok(resolved)
    }

    /// Every owner this context may read anything of. Storage returns only
    /// the rows whose kind each owner's read limit covers.
    #[allow(
        clippy::unused_async,
        reason = "keeps the shared read gate source-compatible with async Engine callers"
    )]
    pub(in crate::engine) async fn authorize_read<A>(
        &self,
        authority: &A,
    ) -> Result<Vec<OwnerRef>, ProtocolError>
    where
        A: EngineAuthority + ?Sized,
    {
        let operation = self.operation_authority(authority)?;
        let authz = operation.authz();
        let read = if authz.auth_path() == AuthPath::Denied {
            Vec::new()
        } else {
            authz.readable_owners(AccessKind::Fact)
        };
        let principal = authz.principal();
        let input = AuthzInput {
            authz,
            requested: &principal,
            resolved: &principal,
            operation: AuthzOperation::Read {
                kind: AccessKind::Fact,
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

    /// Single-entry read gate. Existence is not disclosed to non-readers: a
    /// missing entity and one whose kind the caller may not read are both
    /// [`ENTRY_NOT_FOUND_MESSAGE`].
    pub(in crate::engine) async fn authorize_entry_read<A>(
        &self,
        authority: &A,
        entity: EntityId,
    ) -> Result<EntryReadPermit, ProtocolError>
    where
        A: EngineAuthority + ?Sized,
    {
        let read = self.authorize_read(authority).await?;
        let operation = self.operation_authority(authority)?;
        let authz = operation.authz();
        let owner = self
            .storage()
            .pipeline
            .owner_access_read
            .visible_home_owner(authz.owner_scope(), entity, &read)
            .await
            .map_err(|err| storage_error("visible_home_owner", &err))?
            .and_then(|(owner, kind)| authz.may_read(&owner, kind).then_some(owner))
            .ok_or_else(|| ProtocolError::forbidden(ENTRY_NOT_FOUND_MESSAGE))?;
        Ok(EntryReadPermit { owner })
    }
}

fn allows(authz: &AuthzContext, owner: &OwnerRef, operation: &AuthzOperation) -> bool {
    match *operation {
        AuthzOperation::Read { kind } => authz.may_read(owner, kind),
        AuthzOperation::Write { kind } => authz.may_write(owner, kind),
        AuthzOperation::OwnerAdmin => authz.role_for_owner(owner).is_some_and(Role::administers),
        AuthzOperation::Membership { .. } | AuthzOperation::EntityTransfer { .. } => false,
    }
}

fn denied(operation: &AuthzOperation) -> ProtocolError {
    ProtocolError::forbidden(match operation {
        AuthzOperation::Read { kind } => format!("requires {kind:?} read on this owner"),
        AuthzOperation::Write { kind } => format!("requires {kind:?} write on this owner"),
        _ => "requires admin on this owner".to_owned(),
    })
}

fn storage_error(context: &str, err: &StorageError) -> ProtocolError {
    ProtocolError::internal(format!("{context}: {err}"))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use crate::access::{AccessCeiling, AccessKind, EntityId, Relation, Role};
    use crate::authz::{
        AuthPath, AuthorizationHook, AuthzContext, AuthzInput, AuthzOperation, AuthzOutcome,
        AuthzVeto, OwnerResolver,
    };
    use crate::error::ErrorCode;
    use crate::error::ProtocolError;
    use crate::{FlavorRegistry, GroupId, MemoryId, Owner, OwnerRef, UserId};

    use super::super::MembershipStorage;
    use super::Engine;

    type ResolvedAuthz = AuthzContext;

    fn engine() -> Engine {
        Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests())
    }

    fn engine_from_registry(registry: FlavorRegistry) -> Engine {
        Engine::new(registry.freeze_or_panic_for_tests())
    }

    fn engine_with_ports(storage: MembershipStorage) -> Engine {
        Engine::compose_or_panic_for_tests(storage.storage_ports(), |_| {})
    }

    fn engine_from_registry_and_storage(
        registry: FlavorRegistry,
        storage: MembershipStorage,
    ) -> Engine {
        Engine::new(registry.freeze_or_panic_for_tests())
            .with_storage_ports(storage.storage_ports())
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
            observed_entity_reads: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            observed_kind_loads: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            member,
            group,
            membership_relation,
            home_owner: None,
            entity_readable: false,
            memory_kind: None,
            goal_evidence: None,
            observed_fact_writes: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            observed_modify_evidence: std::sync::Arc::new(std::sync::Mutex::new(None)),
            observed_goal_authorship: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    fn storage_with_entity(
        member: OwnerRef,
        group: GroupId,
        home_owner: Option<OwnerRef>,
        entity_readable: bool,
    ) -> MembershipStorage {
        MembershipStorage {
            observed_entity_reads: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            observed_kind_loads: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            member,
            group,
            membership_relation: Relation::Viewer,
            home_owner,
            entity_readable,
            memory_kind: None,
            goal_evidence: None,
            observed_fact_writes: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            observed_modify_evidence: std::sync::Arc::new(std::sync::Mutex::new(None)),
            observed_goal_authorship: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    fn granted_context(owner: &Owner) -> ResolvedAuthz {
        AuthzContext::single_owner(owner, AuthPath::HostBearer)
    }

    /// Server-resolved caller holding `relation` on `group` (group access now
    /// flows from host-resolved `OwnerRoles`, not from a per-request membership
    /// storage lookup).
    fn member_context(owner: &Owner, group: GroupId, relation: Relation) -> ResolvedAuthz {
        let OwnerRef::Personal(subject) = owner else {
            panic!("member_context requires a personal owner");
        };
        AuthzContext::for_subject_with_role(
            *subject,
            [(OwnerRef::Group(group), relation.role())],
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
            assert_eq!(
                input.operation,
                AuthzOperation::Read {
                    kind: AccessKind::Fact
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
    async fn single_owner_context_reads_its_owner() {
        let engine = engine();
        let owner = owner();
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);

        let resolved = engine
            .authorize_owner_read(&authz, &owner, AccessKind::Goal)
            .await
            .expect("single-owner context should authorize");

        assert_eq!(resolved, owner);
    }

    /// The one rule, at the gate: a role may act on a kind when its limit for
    /// that direction is at least the kind. Reads and writes, every preset.
    #[tokio::test]
    async fn every_gate_follows_the_role_limit() {
        let engine = engine();
        let roles = [
            Role::viewer(),
            Role::ingest(),
            Role::editor(),
            Role::admin(),
            Role::new(AccessCeiling::Goal, AccessCeiling::Abstraction, false).expect("role"),
            Role::new(
                AccessCeiling::Abstraction,
                AccessCeiling::Abstraction,
                false,
            )
            .expect("role"),
        ];
        for role in roles {
            let subject = UserId::new(uuid::Uuid::now_v7());
            let group = OwnerRef::Group(GroupId::new(uuid::Uuid::now_v7()));
            let authz =
                AuthzContext::for_subject_with_role(subject, [(group, role)], AuthPath::HostBearer);
            for kind in AccessKind::ALL {
                let read = engine.authorize_owner_read(&authz, &group, kind).await;
                assert_eq!(read.is_ok(), role.may_read(kind), "{role:?} read {kind:?}");
                let write = engine.authorize_write(&authz, &group, kind).await;
                assert_eq!(
                    write.is_ok(),
                    role.may_write(kind),
                    "{role:?} write {kind:?}"
                );
            }
            let admin = engine.authorize_owner_admin(&authz, &group).await;
            assert_eq!(admin.is_ok(), role.administers(), "{role:?} owner admin");
            let readers = engine
                .authorize_read(&authz)
                .await
                .expect("reads its own owner");
            assert!(readers.contains(&group), "{role:?} reads the group");
        }
    }

    #[tokio::test]
    async fn authorize_write_allows_self_editor() {
        let p = owner();
        let g1 = GroupId::new(uuid::Uuid::now_v7());
        let engine = engine_with_ports(storage(p, g1));
        let authz = granted_context(&p);

        let permit = engine
            .authorize_write(&authz, &p, AccessKind::Goal)
            .await
            .expect("the personal owner writes every kind");

        assert_eq!(permit.owner(), &p);
    }

    #[tokio::test]
    async fn a_system_context_writes_only_when_for_system_built_it() {
        let (engine, authority) = engine().into_system_authority();
        let owner = owner();
        let OwnerRef::Personal(subject) = owner else {
            unreachable!("owner() is personal")
        };
        let roles = crate::OwnerRoles::for_subject(subject, []).expect("roles");

        let permit = engine
            .authorize_owner_write(
                &AuthzContext::for_system(&authority, roles.clone()),
                &owner,
                AccessKind::Perspective,
            )
            .await
            .expect("for_system proves the host holds the witness");
        assert_eq!(permit.owner(), &owner);
        assert_eq!(permit.access_kind(), AccessKind::Perspective);

        let forged = AuthzContext::server_resolved(roles.clone(), AuthPath::System);
        let error = engine
            .authorize_owner_write(&forged, &owner, AccessKind::Fact)
            .await
            .expect_err("a System context not from for_system writes nothing");
        assert_eq!(error.code, ErrorCode::Forbidden);
        assert_eq!(
            error.message,
            "System write authority requires a context from AuthzContext::for_system"
        );

        let (_, foreign) = self::engine().into_system_authority();
        let error = engine
            .authorize_owner_write(
                &AuthzContext::for_system(&foreign, roles),
                &owner,
                AccessKind::Fact,
            )
            .await
            .expect_err("another Engine's witness stays powerless");
        assert_eq!(error.code, ErrorCode::Forbidden);
    }

    #[tokio::test]
    async fn authorize_write_denies_viewer_for_editor() {
        let p = owner();
        let g1 = GroupId::new(uuid::Uuid::now_v7());
        let g1_owner = OwnerRef::Group(g1);
        let engine = engine_with_ports(storage(p, g1));
        let authz = member_context(&p, g1, Relation::Viewer);

        let err = engine
            .authorize_write(&authz, &g1_owner, AccessKind::Fact)
            .await
            .expect_err("a viewer writes nothing");

        assert_eq!(err.code, ErrorCode::Forbidden);
    }

    #[tokio::test]
    async fn authorize_write_allows_editor_member_for_group() {
        let p = owner();
        let g1 = GroupId::new(uuid::Uuid::now_v7());
        let g1_owner = OwnerRef::Group(g1);
        let engine = engine_with_ports(storage_with_relation(p, g1, Relation::Editor));
        let authz = member_context(&p, g1, Relation::Editor);

        let permit = engine
            .authorize_write(&authz, &g1_owner, AccessKind::Perspective)
            .await
            .expect("an editor writes Perspectives");

        assert_eq!(permit.owner(), &g1_owner);
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
            .authorize_write(&authz, &p, AccessKind::Fact)
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
            .authorize_write(&authz, &p, AccessKind::Fact)
            .await
            .expect_err("denied context should reject write authorization");

        assert_eq!(err.code, ErrorCode::Forbidden);
    }

    #[tokio::test]
    async fn authorize_read_returns_personal_and_groups() {
        let p = owner();
        let g1 = GroupId::new(uuid::Uuid::now_v7());
        let g1_owner = OwnerRef::Group(g1);
        let engine = engine_with_ports(storage(p, g1));
        let authz = member_context(&p, g1, Relation::Viewer);

        let read = engine
            .authorize_read(&authz)
            .await
            .expect("member context should resolve read owners");

        assert!(read.contains(&p));
        assert!(read.contains(&g1_owner));
        assert_eq!(read.len(), 2, "no owner beyond the caller's own read set");
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
    async fn denied_context_returns_forbidden() {
        let engine = engine();
        let owner = owner();
        let authz = AuthzContext::denied_for_owner(&owner);

        let err = engine
            .authorize_owner_read(&authz, &owner, AccessKind::Fact)
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
            .authorize_owner_read(&authz, &owner, AccessKind::Fact)
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
        registry
            .set_owner_resolver_or_panic_for_tests(Arc::new(StaticResolver { resolved: hidden }));
        let engine = engine_from_registry(registry);
        let authz = AuthzContext::single_owner(&requested, AuthPath::System);

        let err = engine
            .authorize_owner_read(&authz, &requested, AccessKind::Fact)
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
            .authorize_owner_read(&authz, &owner, AccessKind::Fact)
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
            .authorize_owner_read(&allowed, &owner, AccessKind::Fact)
            .await
            .expect("allowed request should pass");
        let err = engine
            .authorize_owner_read(&denied, &denied_owner, AccessKind::Fact)
            .await
            .expect_err("denied context should reject authorization");

        assert_eq!(err.code, ErrorCode::Forbidden);
        assert_eq!(
            *outcomes.lock().expect("recorder lock"),
            vec![AuthzOutcome::Allowed, AuthzOutcome::DeniedGrant],
        );
    }
}
