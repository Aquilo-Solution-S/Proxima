//! Engine owner erase methods — abandonment-only.

use super::Engine;
use crate::authz::{AuthPath, AuthzContext};
use crate::error::ProtocolError;
use crate::owner_inverse::{
    EraseAuthorization, ExportAuthorization, OwnerEraseContext, OwnerEraseOutcome,
    OwnerEraseRefusal, OwnerEraseTarget, OwnerExportBundle, OwnerExportContext, OwnerExportTarget,
    OwnerSurfaces,
};
use crate::storage_ports::OperatorMaintenanceProof;
use crate::{EmbeddingAnnObservability, EmbeddingOrphanSweepOutcome};
use crate::{GroupId, OwnerRef, SourceId, UserId};

/// The verdict of [`Engine::admit_erase`].
enum EraseAdmission {
    /// Authorized: the token storage requires before it will delete.
    Admitted(EraseAuthorization),
    /// Turned away, and already recorded against the operation id the caller
    /// is handed back.
    Refused(OwnerEraseOutcome),
}

impl Engine {
    /// The one place the owner-inverse lanes learn which tables exist.
    ///
    /// The `owner_pinned` leg used to be appended by the Postgres adapter
    /// from `pg_sidecar!(owner_pinned: true)`, a third source of truth core
    /// could not see. It now comes off the same flavor contracts as the
    /// other four, via `TransferRule::RetainAtSource`, and the adapter's
    /// macro flag is checked against it when the sidecar registry freezes.
    fn owner_surfaces(&self) -> OwnerSurfaces {
        OwnerSurfaces::for_registry(&self.registry)
    }

    fn erase_context(authz: &AuthzContext, target: OwnerEraseTarget) -> OwnerEraseContext {
        OwnerEraseContext::new(
            uuid::Uuid::now_v7(),
            target,
            authz.subject(),
            authz.auth_path(),
            time::OffsetDateTime::now_utc(),
        )
    }

    fn export_context(authz: &AuthzContext, target: OwnerExportTarget) -> OwnerExportContext {
        OwnerExportContext::new(
            uuid::Uuid::now_v7(),
            target,
            authz.subject(),
            authz.auth_path(),
            time::OffsetDateTime::now_utc(),
        )
    }

    pub(in crate::engine) async fn erase_authority_grants(
        &self,
        authz: &AuthzContext,
        target: &OwnerEraseTarget,
    ) -> bool {
        if authz.auth_path() == AuthPath::Delegated {
            return false;
        }
        if authz.auth_path() == AuthPath::System {
            return true;
        }
        let Some(port) = &self.storage.owner_inverse.erase_authority else {
            return false;
        };
        port.may_erase_owner(authz, target).await.unwrap_or(false)
    }

    async fn export_authority_grants(
        &self,
        authz: &AuthzContext,
        target: &OwnerExportTarget,
    ) -> bool {
        if authz.auth_path() == AuthPath::Delegated {
            return false;
        }
        if authz.auth_path() == AuthPath::System {
            return true;
        }
        let Some(port) = &self.storage.owner_inverse.erase_authority else {
            return false;
        };
        port.may_export_owner(authz, target).await.unwrap_or(false)
    }

    async fn operator_maintenance_authorized(&self, authz: &AuthzContext) -> bool {
        if authz.auth_path() == AuthPath::Delegated {
            return false;
        }
        if authz.auth_path() == AuthPath::System {
            return true;
        }
        let Some(port) = &self.storage.owner_inverse.erase_authority else {
            return false;
        };
        port.may_perform_operator_maintenance(authz)
            .await
            .unwrap_or(false)
    }

    /// Owner-agnostic embedding ANN health signals.
    ///
    /// Requires system or owner-erase or operator-maintenance authority.
    ///
    /// # Errors
    ///
    /// Returns forbidden when the caller lacks operator authority, or internal
    /// when the storage read fails.
    pub async fn embedding_ann_observability(
        &self,
        authz: &AuthzContext,
    ) -> Result<EmbeddingAnnObservability, ProtocolError> {
        if !self.operator_maintenance_authorized(authz).await {
            return Err(ProtocolError::forbidden(
                "embedding ANN observability requires operator maintenance authorization",
            ));
        }
        self.storage
            .owner_inverse
            .embedding_maintenance
            .embedding_ann_observability(
                self.embedding_runtime_policy(),
                OperatorMaintenanceProof::new(),
            )
            .await
            .map_err(|e| ProtocolError::internal(format!("embedding_ann_observability: {e}")))
    }

    /// Sweep orphaned embedding infrastructure rows.
    ///
    /// This is crash-residue maintenance only. Owner erase remains
    /// synchronous and cannot rely on this sweep for lawful wipe semantics.
    ///
    /// # Errors
    ///
    /// Returns forbidden when the caller lacks operator authority, or internal
    /// when the storage sweep fails.
    pub async fn sweep_orphan_embedding_rows(
        &self,
        authz: &AuthzContext,
    ) -> Result<EmbeddingOrphanSweepOutcome, ProtocolError> {
        if !self.operator_maintenance_authorized(authz).await {
            return Err(ProtocolError::forbidden(
                "embedding orphan sweep requires operator maintenance authorization",
            ));
        }
        self.storage
            .owner_inverse
            .embedding_maintenance
            .sweep_orphan_embedding_rows(OperatorMaintenanceProof::new())
            .await
            .map_err(|e| ProtocolError::internal(format!("sweep_orphan_embedding_rows: {e}")))
    }

    async fn verify_personal_owner_drop(
        &self,
        user_id: UserId,
        drop_event_id: &str,
    ) -> Result<bool, OwnerEraseRefusal> {
        let Some(port) = &self.storage.owner_inverse.owner_drop_proof else {
            return Err(OwnerEraseRefusal::DropProofPortUnavailable);
        };
        match port
            .verify_personal_owner_dropped(user_id, drop_event_id)
            .await
        {
            Ok(true) => Ok(true),
            Ok(false) | Err(_) => Err(OwnerEraseRefusal::PersonalDropNotVerified),
        }
    }

    /// True iff a cited-object erase port is wired, i.e. an owner-scope erase
    /// will attempt a post-commit object-store purge. The storage verb
    /// persists this planned state as `cited_object_purge_pending` on the
    /// audit row inside the same transaction as the erase, so the durable
    /// record never claims a clean erase while a purge is still outstanding.
    fn owner_object_purge_planned(&self) -> bool {
        self.cited_object_erase().is_some()
    }

    /// Reclaim cited-object payloads from the host-wired object store after an
    /// owner-scope erase completes in Postgres, and report on the receipt
    /// whether a debt is outstanding:
    ///
    /// - no port configured: nothing was planned, so nothing is pending.
    /// - purge succeeds: `false`.
    /// - purge fails: `true`, and the host retries out-of-band.
    ///
    /// There used to be a fourth case — the purge succeeded but clearing the
    /// durable audit flag failed, so the row over-reported forever. There is
    /// no durable flag now: the receipt states what this operation did, and
    /// the host records it if its promises require a record.
    async fn finalize_owner_erase_with_object_purge(
        &self,
        owner: OwnerRef,
        outcome: OwnerEraseOutcome,
    ) -> OwnerEraseOutcome {
        let OwnerEraseOutcome::Completed {
            operation_id,
            counts,
            cold_object_purge_pending,
            ..
        } = outcome
        else {
            return outcome;
        };
        let cited_object_purge_pending = if let Some(port) = self.cited_object_erase() {
            match port.purge_owner_objects(owner).await {
                Ok(purged) => {
                    tracing::debug!(?owner, purged, "owner-scope erase purged cited objects");
                    false
                }
                Err(error) => {
                    tracing::warn!(
                        ?owner,
                        %error,
                        "owner-scope owner erase committed but cited-object purge failed; \
                         object-store cleanup must be retried out-of-band"
                    );
                    true
                }
            }
        } else {
            false
        };
        OwnerEraseOutcome::Completed {
            operation_id,
            counts,
            cited_object_purge_pending,
            cold_object_purge_pending,
        }
    }

    /// The front half of every owner erase: open the audit row, check
    /// owner-erase authority, and — when the target names a drop
    /// event — verify it before storage can receive a deletion token.
    ///
    /// Both refusals carry the operation id the caller is handed back, so an
    /// attempt that deleted nothing is as identifiable as one that deleted
    /// everything. Core writes no trail of either: the receipt goes to the
    /// host, which owns whatever record its own promises require.
    ///
    /// `drop_event` is `Some` exactly for the two personal targets. A group
    /// owner declares no drop event; its abandonment is what storage rechecks
    /// under lock instead.
    async fn admit_erase(
        &self,
        authz: &AuthzContext,
        target: OwnerEraseTarget,
        drop_event: Option<(UserId, &str)>,
    ) -> Result<EraseAdmission, ProtocolError> {
        let audit = Self::erase_context(authz, target.clone());
        let operation_id = audit.operation_id();

        if !self.erase_authority_grants(authz, &target).await {
            return Ok(EraseAdmission::Refused(OwnerEraseOutcome::Unauthorized {
                operation_id,
            }));
        }

        if let Some((user_id, drop_event_id)) = drop_event
            && let Err(reason) = self
                .verify_personal_owner_drop(user_id, drop_event_id)
                .await
        {
            return Ok(EraseAdmission::Refused(OwnerEraseOutcome::Refused {
                operation_id,
                reason,
            }));
        }

        Ok(EraseAdmission::Admitted(EraseAuthorization::new(audit)))
    }

    /// Export one owner's owner bundle.
    ///
    /// Requires owner-erase authority. Export is
    /// non-destructive: personal-owner drop proof does not gate it.
    ///
    /// # Errors
    ///
    /// Returns forbidden when the caller lacks owner-erase authority,
    /// or internal when storage export fails.
    pub async fn export_owner_bundle(
        &self,
        authz: &AuthzContext,
        target: OwnerExportTarget,
    ) -> Result<OwnerExportBundle, ProtocolError> {
        if !self.export_authority_grants(authz, &target).await {
            return Err(ProtocolError::forbidden(
                "owner export requires owner-erase authority",
            ));
        }

        let auth = ExportAuthorization::new(Self::export_context(authz, target));
        let surfaces = self.owner_surfaces();
        self.storage
            .owner_inverse
            .owner_erase
            .export_owner_bundle(&auth, &surfaces)
            .await
            .map_err(|e| ProtocolError::internal(format!("export_owner_bundle: {e}")))
    }

    /// Erase an abandoned group owner and all its owned rows.
    ///
    /// Requires `OwnerEraseAuthorityPort` approval or [`AuthPath::System`].
    /// Storage rechecks abandonment under lock before deleting.
    ///
    /// # Errors
    ///
    /// Returns a protocol error when audit recording or storage execution fails.
    pub async fn erase_group_owner(
        &self,
        authz: &AuthzContext,
        group_id: GroupId,
    ) -> Result<OwnerEraseOutcome, ProtocolError> {
        let auth = match self
            .admit_erase(authz, OwnerEraseTarget::GroupOwner { group_id }, None)
            .await?
        {
            EraseAdmission::Admitted(auth) => auth,
            EraseAdmission::Refused(outcome) => return Ok(outcome),
        };
        let surfaces = self.owner_surfaces();
        let object_purge_planned = self.owner_object_purge_planned();
        let outcome = self
            .storage
            .owner_inverse
            .owner_erase
            .erase_group_owner(&auth, group_id, object_purge_planned, &surfaces)
            .await
            .map_err(|e| ProtocolError::internal(format!("erase_group_owner: {e}")))?;
        Ok(self
            .finalize_owner_erase_with_object_purge(OwnerRef::Group(group_id), outcome)
            .await)
    }

    /// Erase a dropped personal owner and all its owned rows.
    ///
    /// Requires `OwnerEraseAuthorityPort`/system authorization and trusted host
    /// drop proof before storage can receive a deletion token.
    ///
    /// # Errors
    ///
    /// Returns a protocol error when audit recording, drop-proof access, or storage execution fails.
    pub async fn erase_personal_owner(
        &self,
        authz: &AuthzContext,
        user_id: UserId,
        drop_event_id: String,
    ) -> Result<OwnerEraseOutcome, ProtocolError> {
        let target = OwnerEraseTarget::PersonalOwner {
            user_id,
            drop_event_id: drop_event_id.clone(),
        };
        let auth = match self
            .admit_erase(authz, target, Some((user_id, &drop_event_id)))
            .await?
        {
            EraseAdmission::Admitted(auth) => auth,
            EraseAdmission::Refused(outcome) => return Ok(outcome),
        };
        let surfaces = self.owner_surfaces();
        let object_purge_planned = self.owner_object_purge_planned();
        let outcome = self
            .storage
            .owner_inverse
            .owner_erase
            .erase_personal_owner(&auth, user_id, object_purge_planned, &surfaces)
            .await
            .map_err(|e| ProtocolError::internal(format!("erase_personal_owner: {e}")))?;
        Ok(self
            .finalize_owner_erase_with_object_purge(OwnerRef::Personal(user_id), outcome)
            .await)
    }

    /// Erase one source scope for an abandoned group owner.
    /// Storage rechecks abandonment under lock before deleting.
    ///
    /// # Errors
    ///
    /// Returns a protocol error when audit recording or storage execution fails.
    pub async fn erase_group_source_scope(
        &self,
        authz: &AuthzContext,
        group_id: GroupId,
        source_id: SourceId,
    ) -> Result<OwnerEraseOutcome, ProtocolError> {
        let target = OwnerEraseTarget::GroupSourceScope {
            group_id,
            source_id: source_id.clone(),
        };
        let auth = match self.admit_erase(authz, target, None).await? {
            EraseAdmission::Admitted(auth) => auth,
            EraseAdmission::Refused(outcome) => return Ok(outcome),
        };
        let surfaces = self.owner_surfaces();
        // Source-scope blob purge is deferred: a source scope is a partial
        // owner, and the object store keys purely by owner, so a prefix-delete
        // would over-delete the owner's other sources. Only owner-scope erase
        // purges the object store.
        self.storage
            .owner_inverse
            .owner_erase
            .erase_group_source_scope(&auth, group_id, &source_id, &surfaces)
            .await
            .map_err(|e| ProtocolError::internal(format!("erase_group_source_scope: {e}")))
    }

    /// Erase one source scope for a dropped personal owner.
    ///
    /// # Errors
    ///
    /// Returns a protocol error when audit recording, drop-proof access, or storage execution fails.
    pub async fn erase_personal_source_scope(
        &self,
        authz: &AuthzContext,
        user_id: UserId,
        source_id: SourceId,
        drop_event_id: String,
    ) -> Result<OwnerEraseOutcome, ProtocolError> {
        let target = OwnerEraseTarget::PersonalSourceScope {
            user_id,
            source_id: source_id.clone(),
            drop_event_id: drop_event_id.clone(),
        };
        let auth = match self
            .admit_erase(authz, target, Some((user_id, &drop_event_id)))
            .await?
        {
            EraseAdmission::Admitted(auth) => auth,
            EraseAdmission::Refused(outcome) => return Ok(outcome),
        };
        let surfaces = self.owner_surfaces();
        // Source-scope blob purge is deferred (see
        // `erase_group_source_scope`): prefix-delete keys by owner,
        // so it would over-delete the owner's other sources.
        self.storage
            .owner_inverse
            .owner_erase
            .erase_personal_source_scope(&auth, user_id, &source_id, &surfaces)
            .await
            .map_err(|e| ProtocolError::internal(format!("erase_personal_source_scope: {e}")))
    }
}

#[cfg(test)]
mod purge_tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;

    use super::Engine;
    use crate::access::AccessError;
    use crate::authz::{AuthPath, AuthzContext};
    use crate::owner_inverse::{
        EraseAuthorization, ExportAuthorization, OwnerEraseCounts, OwnerEraseOutcome,
        OwnerEraseRefusal, OwnerExportBundle,
    };
    use crate::storage::StorageError;
    use crate::storage_ports::{
        CitedObjectErasePort, OwnerDropProofPort, OwnerEraseAuthorityPort, OwnerInversePort,
        StoragePorts,
    };
    use crate::{GroupId, OwnerRef, SourceId, UserId};

    #[derive(Debug, Default)]
    struct PermissiveAdmin {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl OwnerEraseAuthorityPort for PermissiveAdmin {
        async fn may_erase_owner(
            &self,
            _authz: &AuthzContext,
            _target: &crate::owner_inverse::OwnerEraseTarget,
        ) -> Result<bool, AccessError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(true)
        }

        async fn may_perform_operator_maintenance(
            &self,
            _authz: &AuthzContext,
        ) -> Result<bool, AccessError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(true)
        }
    }

    /// `OwnerInversePort` whose erase verbs return a fixed outcome.
    #[derive(Debug)]
    struct FixedOutcomeErase {
        outcome: OwnerEraseOutcome,
        /// Bumped by every erase verb, so a test can assert an attempt was
        /// turned away before storage rather than merely reporting a refusal.
        erase_calls: AtomicUsize,
    }

    impl FixedOutcomeErase {
        fn completed() -> Self {
            Self::completed_outcome()
        }

        fn completed_with_cold_pending() -> Self {
            Self {
                outcome: OwnerEraseOutcome::Completed {
                    operation_id: uuid::Uuid::now_v7(),
                    counts: OwnerEraseCounts::default(),
                    cited_object_purge_pending: false,
                    cold_object_purge_pending: true,
                },
                erase_calls: AtomicUsize::new(0),
            }
        }

        fn completed_outcome() -> Self {
            Self {
                outcome: OwnerEraseOutcome::Completed {
                    operation_id: uuid::Uuid::now_v7(),
                    counts: OwnerEraseCounts::default(),
                    cited_object_purge_pending: false,
                    cold_object_purge_pending: false,
                },
                erase_calls: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl OwnerInversePort for FixedOutcomeErase {
        async fn erase_group_owner(
            &self,
            _auth: &EraseAuthorization,
            _group_id: GroupId,
            _object_purge_planned: bool,
            _tables: &crate::owner_inverse::OwnerSurfaces,
        ) -> Result<OwnerEraseOutcome, StorageError> {
            self.erase_calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.outcome.clone())
        }

        async fn erase_personal_owner(
            &self,
            _auth: &EraseAuthorization,
            _user_id: UserId,
            _object_purge_planned: bool,
            _tables: &crate::owner_inverse::OwnerSurfaces,
        ) -> Result<OwnerEraseOutcome, StorageError> {
            self.erase_calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.outcome.clone())
        }

        async fn erase_group_source_scope(
            &self,
            _auth: &EraseAuthorization,
            _group_id: GroupId,
            _source_id: &SourceId,
            _tables: &crate::owner_inverse::OwnerSurfaces,
        ) -> Result<OwnerEraseOutcome, StorageError> {
            self.erase_calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.outcome.clone())
        }

        async fn erase_personal_source_scope(
            &self,
            _auth: &EraseAuthorization,
            _user_id: UserId,
            _source_id: &SourceId,
            _tables: &crate::owner_inverse::OwnerSurfaces,
        ) -> Result<OwnerEraseOutcome, StorageError> {
            self.erase_calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.outcome.clone())
        }

        async fn export_owner_bundle(
            &self,
            _auth: &ExportAuthorization,
            _tables: &crate::owner_inverse::OwnerSurfaces,
        ) -> Result<OwnerExportBundle, StorageError> {
            Err(StorageError::Internal("export not used in test".into()))
        }
    }

    /// Records every owner it was asked to purge; optionally fails every call.
    #[derive(Debug, Default)]
    struct RecordingPurge {
        purged: Mutex<Vec<OwnerRef>>,
        fail: bool,
    }

    #[async_trait]
    impl CitedObjectErasePort for RecordingPurge {
        async fn purge_owner_objects(&self, owner: OwnerRef) -> Result<u64, StorageError> {
            self.purged.lock().unwrap().push(owner);
            if self.fail {
                Err(StorageError::Unavailable("s3 unavailable".into()))
            } else {
                Ok(3)
            }
        }
    }

    /// `OwnerDropProofPort` that always verifies the drop (personal-owner path).
    #[derive(Debug)]
    struct DropVerified;

    #[async_trait]
    impl OwnerDropProofPort for DropVerified {
        async fn verify_personal_owner_dropped(
            &self,
            _user_id: UserId,
            _drop_event_id: &str,
        ) -> Result<bool, AccessError> {
            Ok(true)
        }
    }

    /// `OwnerDropProofPort` that answers "this owner was not dropped".
    #[derive(Debug)]
    struct DropUnverified;

    #[async_trait]
    impl OwnerDropProofPort for DropUnverified {
        async fn verify_personal_owner_dropped(
            &self,
            _user_id: UserId,
            _drop_event_id: &str,
        ) -> Result<bool, AccessError> {
            Ok(false)
        }
    }

    fn system_authz() -> AuthzContext {
        AuthzContext::for_subject(UserId::new(uuid::Uuid::now_v7()), AuthPath::System)
    }

    fn engine_with(
        erase: Arc<FixedOutcomeErase>,
        drop_proof: Option<Arc<DropVerified>>,
        purge: Arc<RecordingPurge>,
    ) -> Engine {
        let drop_proof = drop_proof.map(|d| d as Arc<dyn OwnerDropProofPort>);
        let storage = StoragePorts::rejecting_with_owner_inverse(erase, drop_proof);
        Engine::compose_or_panic_for_tests(storage, |_| {}).with_cited_object_erase(purge)
    }

    #[tokio::test]
    async fn raw_delegated_maintenance_is_denied_before_admin_or_storage() {
        let admin = Arc::new(PermissiveAdmin::default());
        let storage = StoragePorts::rejecting_with_erase_authority(admin.clone());
        let engine = Engine::compose_or_panic_for_tests(storage, |_| {});
        let subject = UserId::new(uuid::Uuid::now_v7());
        let owner = OwnerRef::Group(GroupId::new(uuid::Uuid::now_v7()));
        let raw = AuthzContext::for_subject_with_role(
            subject,
            [(owner, crate::Role::admin())],
            AuthPath::Delegated,
        );

        let error = engine
            .sweep_orphan_embedding_rows(&raw)
            .await
            .expect_err("raw delegated authority must not reach maintenance ports");

        assert_eq!(error.code, crate::ErrorCode::Forbidden);
        assert_eq!(admin.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn completed_group_owner_erase_purges_once_with_owner() {
        let group = GroupId::new(uuid::Uuid::now_v7());
        let purge = Arc::new(RecordingPurge::default());
        let erase = Arc::new(FixedOutcomeErase::completed());
        let engine = engine_with(erase.clone(), None, purge.clone());

        let outcome = engine
            .erase_group_owner(&system_authz(), group)
            .await
            .expect("erase returns an outcome");

        let OwnerEraseOutcome::Completed {
            cited_object_purge_pending,
            ..
        } = outcome
        else {
            panic!("expected completed erase, got {outcome:?}");
        };
        assert!(
            !cited_object_purge_pending,
            "a successful purge leaves no debt on the receipt"
        );
        assert_eq!(
            purge.purged.lock().unwrap().as_slice(),
            &[OwnerRef::Group(group)]
        );
    }

    #[tokio::test]
    async fn completed_personal_owner_erase_purges_once_with_owner() {
        let user = UserId::new(uuid::Uuid::now_v7());
        let purge = Arc::new(RecordingPurge::default());
        let erase = Arc::new(FixedOutcomeErase::completed());
        let engine = engine_with(erase.clone(), Some(Arc::new(DropVerified)), purge.clone());

        let outcome = engine
            .erase_personal_owner(&system_authz(), user, "drop-1".into())
            .await
            .expect("erase returns an outcome");

        let OwnerEraseOutcome::Completed {
            cited_object_purge_pending,
            ..
        } = outcome
        else {
            panic!("expected completed erase, got {outcome:?}");
        };
        assert!(
            !cited_object_purge_pending,
            "a successful purge leaves no debt on the receipt"
        );
        assert_eq!(
            purge.purged.lock().unwrap().as_slice(),
            &[OwnerRef::Personal(user)]
        );
    }

    #[tokio::test]
    async fn cited_object_finalization_preserves_independent_cold_purge_state() {
        let group = GroupId::new(uuid::Uuid::now_v7());
        let purge = Arc::new(RecordingPurge::default());
        let erase = Arc::new(FixedOutcomeErase::completed_with_cold_pending());
        let engine = engine_with(erase, None, purge);

        let outcome = engine
            .erase_group_owner(&system_authz(), group)
            .await
            .expect("erase returns an outcome");
        assert!(matches!(
            outcome,
            OwnerEraseOutcome::Completed {
                cited_object_purge_pending: false,
                cold_object_purge_pending: true,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn completed_source_scope_erase_does_not_purge() {
        let group = GroupId::new(uuid::Uuid::now_v7());
        let source = SourceId::new("source/a");
        let purge = Arc::new(RecordingPurge::default());
        // A source scope is a partial owner; even a Completed outcome must not
        // trigger an owner-wide prefix purge.
        let engine = engine_with(
            Arc::new(FixedOutcomeErase::completed()),
            None,
            purge.clone(),
        );

        let outcome = engine
            .erase_group_source_scope(&system_authz(), group, source)
            .await
            .expect("source-scope erase completes");

        assert!(matches!(outcome, OwnerEraseOutcome::Completed { .. }));
        assert!(purge.purged.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn purge_error_does_not_fail_completed_erase() {
        let group = GroupId::new(uuid::Uuid::now_v7());
        let purge = Arc::new(RecordingPurge {
            purged: Mutex::default(),
            fail: true,
        });
        let erase = Arc::new(FixedOutcomeErase::completed());
        let engine = engine_with(erase.clone(), None, purge.clone());

        let outcome = engine
            .erase_group_owner(&system_authz(), group)
            .await
            .expect("best-effort purge failure must not fail the erase");

        assert!(
            matches!(
                outcome,
                OwnerEraseOutcome::Completed {
                    cited_object_purge_pending: true,
                    ..
                }
            ),
            "purge failure must surface cited_object_purge_pending on Completed"
        );
        // Attempted exactly once even though it errored.
        assert_eq!(purge.purged.lock().unwrap().len(), 1);
    }

    /// Drive all four erase entry points against one engine, so a gate that
    /// only some of them apply cannot pass.
    async fn attempt_every_scope(engine: &Engine, authz: &AuthzContext) -> Vec<OwnerEraseOutcome> {
        let group = GroupId::new(uuid::Uuid::now_v7());
        let user = UserId::new(uuid::Uuid::now_v7());
        let source = SourceId::new("source/a");
        vec![
            engine
                .erase_group_owner(authz, group)
                .await
                .expect("group owner erase returns an outcome"),
            engine
                .erase_personal_owner(authz, user, "drop-1".into())
                .await
                .expect("personal owner erase returns an outcome"),
            engine
                .erase_group_source_scope(authz, group, source.clone())
                .await
                .expect("group source scope erase returns an outcome"),
            engine
                .erase_personal_source_scope(authz, user, source, "drop-1".into())
                .await
                .expect("personal source scope erase returns an outcome"),
        ]
    }

    #[tokio::test]
    async fn every_scope_turns_an_unauthorized_attempt_away_before_storage() {
        // No `OwnerEraseAuthorityPort` and a caller who is not `System`: nothing
        // can vouch for this attempt, so none of the four may reach storage.
        let erase = Arc::new(FixedOutcomeErase::completed());
        let engine = engine_with(
            erase.clone(),
            Some(Arc::new(DropVerified)),
            Arc::new(RecordingPurge::default()),
        );
        let authz =
            AuthzContext::for_subject(UserId::new(uuid::Uuid::now_v7()), AuthPath::HostBearer);

        let outcomes = attempt_every_scope(&engine, &authz).await;

        for outcome in &outcomes {
            assert!(
                matches!(outcome, OwnerEraseOutcome::Unauthorized { .. }),
                "expected an unauthorized outcome, got {outcome:?}"
            );
        }
        assert_eq!(
            erase.erase_calls.load(Ordering::SeqCst),
            0,
            "an unauthorized attempt must be turned away before storage"
        );
        for outcome in &outcomes {
            let (OwnerEraseOutcome::Unauthorized { operation_id }
            | OwnerEraseOutcome::Refused { operation_id, .. }) = outcome
            else {
                panic!("expected a refusal carrying an operation id, got {outcome:?}");
            };
            assert!(
                !operation_id.is_nil(),
                "a refusal the host must answer for still names the operation"
            );
        }
    }

    #[tokio::test]
    async fn the_personal_scopes_refuse_before_storage_without_a_verified_drop() {
        // Authorized, but the drop the request names is not one the host will
        // confirm. Both personal scopes must stop; both group scopes, which
        // name no drop event, must still reach storage.
        for (drop_proof, expected) in [
            (
                Some(Arc::new(DropUnverified)),
                OwnerEraseRefusal::PersonalDropNotVerified,
            ),
            (None, OwnerEraseRefusal::DropProofPortUnavailable),
        ] {
            let erase = Arc::new(FixedOutcomeErase::completed());
            let drop_proof = drop_proof.map(|d| d as Arc<dyn OwnerDropProofPort>);
            let storage = StoragePorts::rejecting_with_owner_inverse(erase.clone(), drop_proof);
            let engine = Engine::compose_or_panic_for_tests(storage, |_| {});

            let outcomes = attempt_every_scope(&engine, &system_authz()).await;

            for outcome in [&outcomes[1], &outcomes[3]] {
                let OwnerEraseOutcome::Refused { reason, .. } = outcome else {
                    panic!("expected a refusal for a personal scope, got {outcome:?}");
                };
                assert_eq!(*reason, expected);
            }
            assert_eq!(
                erase.erase_calls.load(Ordering::SeqCst),
                2,
                "only the two group scopes may reach storage without a drop event"
            );
        }
    }
}
