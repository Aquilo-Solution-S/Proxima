use crate::access::{AccessKind, EntityId, Relation};
use crate::authz::{
    AuthPath, AuthzContext, AuthzInput, AuthzOperation, AuthzOutcome, EngineAuthority,
    SystemAuthority,
};
use crate::error::ProtocolError;
use crate::storage::StorageError;
use crate::storage_ports::OwnerWritePermit;
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
    owner_write: Option<OwnerWritePermit>,
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
    relation: Relation,
    owner_write: OwnerWritePermit,
}

impl WritePermit {
    #[must_use]
    pub fn owner(&self) -> &OwnerRef {
        self.owner_write.owner()
    }

    #[must_use]
    pub fn relation(&self) -> Relation {
        self.relation
    }

    #[must_use]
    pub const fn owner_write_permit(&self) -> &OwnerWritePermit {
        &self.owner_write
    }
}

impl From<WritePermit> for MemoryPermit {
    fn from(permit: WritePermit) -> Self {
        let owner = *permit.owner_write.owner();
        Self::owner_scoped_with_write(permit.owner_write, owner, permit.relation)
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
            owner_write: None,
        }
    }

    fn owner_scoped_with_write(
        owner_write: OwnerWritePermit,
        requested: Owner,
        relation: Relation,
    ) -> Self {
        let owner = *owner_write.owner();
        Self {
            mode: PermitMode::OwnerScoped,
            owner,
            requested,
            relation,
            owner_write: Some(owner_write),
        }
    }

    /// Test-only owner-scoped write permit. The gates in this module remain
    /// the production mint; see [`crate::verbs::fact_ingest::AuthorizedFactWrite::new_for_tests`].
    #[cfg(any(test, feature = "test-fixtures"))]
    #[must_use]
    pub fn owner_scoped_with_write_for_tests(
        owner_write: OwnerWritePermit,
        relation: Relation,
    ) -> Self {
        let requested = *owner_write.owner();
        Self::owner_scoped_with_write(owner_write, requested, relation)
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
    pub fn owner_write_permit(&self) -> Option<&OwnerWritePermit> {
        self.owner_write.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn expire_delegated_write_for_test(&mut self) {
        if let Some(owner_write) = &mut self.owner_write {
            owner_write.expire_delegated_for_test();
        }
    }
}

impl Engine {
    /// Storage-tier owner-write gate. `System` contexts require the
    /// boot-time witness; membership-backed and dev-token contexts do not.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when `authz` cannot write `owner`, or when `authz`
    /// uses `System` without a runtime witness.
    pub async fn authorize_owner_write(
        &self,
        authz: &AuthzContext,
        owner: &OwnerRef,
        kind: AccessKind,
    ) -> Result<OwnerWritePermit, ProtocolError> {
        let operation = self.operation_authority(authz)?;
        self.authorize_owner_write_inner(
            operation.authz(),
            owner,
            kind,
            None,
            operation.redeemed_phase(),
        )
        .await
    }

    /// Storage-tier owner-write gate for host-held System authority.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when `authz` cannot write `owner`.
    pub async fn authorize_owner_write_with_system_authority(
        &self,
        authz: &AuthzContext,
        owner: &OwnerRef,
        kind: AccessKind,
        authority: &SystemAuthority,
    ) -> Result<OwnerWritePermit, ProtocolError> {
        if !authority.authorizes(&self.system_authority_binding) {
            return Err(ProtocolError::forbidden(
                "SystemAuthority belongs to a different engine instance",
            ));
        }
        self.authorize_owner_write_inner(authz, owner, kind, Some(authority), false)
            .await
    }

    /// Targeted write/config gate over the resolved access sets.
    pub(in crate::engine) async fn authorize_write<A>(
        &self,
        authority: &A,
        owner: &OwnerRef,
        required: Relation,
    ) -> Result<WritePermit, ProtocolError>
    where
        A: EngineAuthority + ?Sized,
    {
        let kind = access_kind_for_write_relation(required)?;
        let operation = self.operation_authority(authority)?;
        let owner_write = self
            .authorize_owner_write_inner(
                operation.authz(),
                owner,
                kind,
                None,
                operation.redeemed_phase(),
            )
            .await?;
        Ok(WritePermit {
            relation: required,
            owner_write,
        })
    }

    #[allow(
        clippy::unused_async,
        reason = "keeps the owner-write gate on the async Engine authorization seam"
    )]
    async fn authorize_owner_write_inner(
        &self,
        authz: &AuthzContext,
        owner: &OwnerRef,
        kind: AccessKind,
        system_authority: Option<&SystemAuthority>,
        redeemed_phase: bool,
    ) -> Result<OwnerWritePermit, ProtocolError> {
        let required = write_relation_for_access_kind(kind);
        if authz.auth_path() == AuthPath::Delegated && !redeemed_phase {
            return Err(ProtocolError::forbidden(
                "raw delegated authorization contexts are not Engine authority",
            ));
        }
        if authz.auth_path() == AuthPath::System && system_authority.is_none() {
            let input = AuthzInput {
                authz,
                requested: owner,
                resolved: owner,
                relation: required,
                operation: AuthzOperation::Relation { relation: required },
            };
            self.registry
                .run_authorization_observers(&input, AuthzOutcome::DeniedGrant);
            return Err(ProtocolError::forbidden(
                "System write authority requires a runtime SystemAuthority witness",
            ));
        }

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

        let access = self.resolve_access_inner(authz, redeemed_phase)?;
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
        if redeemed_phase {
            let expires_at = authz.expires_at().ok_or_else(|| {
                ProtocolError::forbidden("delegated worker phase has no finite expiry")
            })?;
            Ok(OwnerWritePermit::new_delegated(
                resolved,
                kind,
                self.delegation_runtime_binding.clone(),
                expires_at,
            ))
        } else {
            Ok(OwnerWritePermit::new(resolved, kind))
        }
    }

    /// Read gate returning the resolved owner set visible to this context.
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
        let access = self.resolve_access_inner(authz, operation.redeemed_phase())?;
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
    pub(in crate::engine) async fn authorize_entry_read<A>(
        &self,
        authority: &A,
        entity: EntityId,
    ) -> Result<EntryReadPermit, ProtocolError>
    where
        A: EngineAuthority + ?Sized,
    {
        let read = self.authorize_read(authority).await?;
        let home = self
            .storage()
            .pipeline
            .owner_access_read
            .visible_home_owner(entity, &read)
            .await
            .map_err(|err| storage_error("visible_home_owner", &err))?
            .ok_or_else(|| ProtocolError::forbidden(ENTRY_NOT_FOUND_MESSAGE))?;

        Ok(EntryReadPermit { owner: home })
    }

    /// The one Tier-2 owner/space-scoped gate. Async because relation
    /// resolution reads persisted grants (skipped for Unrestricted).
    /// `denied` contexts are rejected before any resolution.
    pub(in crate::engine) async fn authorize_request<A>(
        &self,
        authority: &A,
        requested: &Owner,
        relation: Relation,
    ) -> Result<MemoryPermit, ProtocolError>
    where
        A: EngineAuthority + ?Sized,
    {
        let operation = self.operation_authority(authority)?;
        let authz = operation.authz();
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
        match self
            .gate_and_veto(
                authz,
                operation.redeemed_phase(),
                &resolved,
                relation,
                &input,
            )
            .await
        {
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
        redeemed_phase: bool,
        owner: &Owner,
        relation: Relation,
        input: &AuthzInput<'_>,
    ) -> Result<AccessBasis, (ProtocolError, AuthzOutcome)> {
        let basis = self
            .resolve_relation(authz, redeemed_phase, owner, relation)
            .await
            .map_err(|err| (err, AuthzOutcome::DeniedGrant))?;
        self.registry
            .run_authorization_vetoes(input)
            .map_err(|err| (err, AuthzOutcome::DeniedVeto))?;
        Ok(basis)
    }

    /// Relation gate over the server-resolved owner access sets.
    #[allow(
        clippy::unused_async,
        reason = "keeps relation resolution on the async authorization pipeline seam"
    )]
    async fn resolve_relation(
        &self,
        authz: &AuthzContext,
        redeemed_phase: bool,
        owner: &Owner,
        relation: Relation,
    ) -> Result<AccessBasis, ProtocolError> {
        let access = self.resolve_access_inner(authz, redeemed_phase)?;
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

fn access_kind_for_write_relation(relation: Relation) -> Result<AccessKind, ProtocolError> {
    match relation {
        Relation::Ingest => Ok(AccessKind::Fact),
        Relation::Editor => Ok(AccessKind::Perspective),
        Relation::Admin => Ok(AccessKind::Goal),
        Relation::Viewer => Err(ProtocolError::invalid_argument(
            "relation",
            "Viewer is not a write relation",
        )),
    }
}

const fn write_relation_for_access_kind(kind: AccessKind) -> Relation {
    match kind {
        AccessKind::Fact => Relation::Ingest,
        AccessKind::Abstraction | AccessKind::Perspective => Relation::Editor,
        AccessKind::Goal => Relation::Admin,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use crate::access::{AccessKind, EntityId, Relation};
    use crate::authz::{
        AuthPath, AuthorizationHook, AuthzContext, AuthzInput, AuthzOperation, AuthzOutcome,
        AuthzVeto, OwnerResolver,
    };
    use crate::error::ErrorCode;
    use crate::error::ProtocolError;
    use crate::{FlavorRegistry, GroupId, MemoryId, Owner, OwnerRef, UserId};

    use super::super::access_sets::tests::MembershipStorage;
    use super::{AccessBasis, Engine, PermitMode};

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
    async fn authorize_owner_write_denies_system_without_authority() {
        let engine = engine();
        let owner = owner();
        let authz = AuthzContext::single_owner(&owner, AuthPath::System);

        let err = engine
            .authorize_owner_write(&authz, &owner, AccessKind::Fact)
            .await
            .expect_err("plain System auth must not mint storage write permits");

        assert_eq!(err.code, ErrorCode::Forbidden);
        assert_eq!(
            err.message,
            "System write authority requires a runtime SystemAuthority witness"
        );
    }

    #[tokio::test]
    async fn authorize_owner_write_allows_system_with_authority() {
        let (engine, authority) = engine().into_system_authority();
        let owner = owner();
        let authz = AuthzContext::single_owner(&owner, AuthPath::System);

        let permit = engine
            .authorize_owner_write_with_system_authority(
                &authz,
                &owner,
                AccessKind::Perspective,
                &authority,
            )
            .await
            .expect("host-held SystemAuthority should admit System writes");

        assert_eq!(permit.owner(), &owner);
        assert_eq!(permit.access_kind(), AccessKind::Perspective);
    }

    #[tokio::test]
    async fn authorize_owner_write_rejects_another_engines_system_authority() {
        let (target_engine, _) = engine().into_system_authority();
        let (_, foreign_authority) = engine().into_system_authority();
        let owner = owner();
        let authz = AuthzContext::single_owner(&owner, AuthPath::System);

        let error = target_engine
            .authorize_owner_write_with_system_authority(
                &authz,
                &owner,
                AccessKind::Perspective,
                &foreign_authority,
            )
            .await
            .expect_err("a witness from another Engine must remain powerless");

        assert_eq!(error.code, ErrorCode::Forbidden);
        assert_eq!(
            error.message,
            "SystemAuthority belongs to a different engine instance"
        );
    }

    #[tokio::test]
    async fn authorize_write_denies_viewer_for_editor() {
        let p = owner();
        let g1 = GroupId::new(uuid::Uuid::now_v7());
        let g1_owner = OwnerRef::Group(g1);
        let engine = engine_with_ports(storage(p, g1));
        let authz = member_context(&p, g1, Relation::Viewer);

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
        let authz = member_context(&p, g1, Relation::Editor);

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
    async fn resolve_relation_reports_acting_as_owner_for_single_owner_context() {
        let engine = engine();
        let owner = owner();
        let authz = AuthzContext::single_owner(&owner, AuthPath::System);

        let basis = engine
            .resolve_relation(&authz, false, &owner, Relation::Viewer)
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
        registry
            .set_owner_resolver_or_panic_for_tests(Arc::new(StaticResolver { resolved: hidden }));
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
