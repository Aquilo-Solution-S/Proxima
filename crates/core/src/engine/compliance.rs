//! Engine compliance erasure methods — abandonment-only.

use super::Engine;
use crate::authz::{AuthPath, AuthzContext};
use crate::compliance::{
    ComplianceAuditContext, ComplianceEraseOutcome, ComplianceEraseRefusal, ComplianceEraseTarget,
    ComplianceExportAuditContext, ComplianceExportBundle, ComplianceExportTarget,
    EraseAuthorization, ExportAuthorization,
};
use crate::error::ProtocolError;
use crate::sidecar_tables;
use crate::storage_ports::OperatorMaintenanceProof;
use crate::verbs::schema::PayloadKind;
use crate::{EmbeddingAnnObservability, EmbeddingOrphanSweepOutcome};
use crate::{GroupId, OwnerRef, SourceId, UserId};

/// The verdict of [`Engine::admit_erase`].
enum EraseAdmission {
    /// Authorized: the token storage requires before it will delete.
    Admitted(EraseAuthorization),
    /// Turned away, and already recorded against the operation id the caller
    /// is handed back.
    Refused(ComplianceEraseOutcome),
}

struct ComplianceSidecarTables {
    fact: Vec<String>,
    goal: Vec<String>,
    citation_mapping: Vec<String>,
    cited_object: Vec<String>,
}

impl Engine {
    fn compliance_memory_sidecar_tables(&self) -> Vec<String> {
        let mut tables = sidecar_tables(self.registry.schemas(), PayloadKind::Fact);
        tables.extend(sidecar_tables(
            self.registry.schemas(),
            PayloadKind::Abstraction,
        ));
        tables.extend(sidecar_tables(
            self.registry.schemas(),
            PayloadKind::Perspective,
        ));
        tables.sort();
        tables.dedup();
        tables
    }

    fn compliance_sidecar_tables(&self) -> ComplianceSidecarTables {
        ComplianceSidecarTables {
            fact: self.compliance_memory_sidecar_tables(),
            goal: sidecar_tables(self.registry.schemas(), PayloadKind::Goal),
            citation_mapping: sidecar_tables(self.registry.schemas(), PayloadKind::CitationMapping),
            cited_object: sidecar_tables(self.registry.schemas(), PayloadKind::CitedObject),
        }
    }

    fn compliance_audit_context(
        authz: &AuthzContext,
        target: ComplianceEraseTarget,
    ) -> ComplianceAuditContext {
        ComplianceAuditContext::new(
            uuid::Uuid::now_v7(),
            target,
            authz.subject(),
            authz.auth_path(),
            time::OffsetDateTime::now_utc(),
        )
    }

    fn compliance_export_audit_context(
        authz: &AuthzContext,
        target: ComplianceExportTarget,
    ) -> ComplianceExportAuditContext {
        ComplianceExportAuditContext::new(
            uuid::Uuid::now_v7(),
            target,
            authz.subject(),
            authz.auth_path(),
            time::OffsetDateTime::now_utc(),
        )
    }

    async fn record_pre_storage_compliance_outcome(
        &self,
        audit: &ComplianceAuditContext,
        outcome: &ComplianceEraseOutcome,
    ) -> Result<(), ProtocolError> {
        self.storage
            .compliance
            .compliance_erase
            .record_compliance_outcome(audit, outcome)
            .await
            .map_err(|e| ProtocolError::internal(format!("record_compliance_outcome: {e}")))
    }

    async fn pre_storage_outcome(
        &self,
        audit: &ComplianceAuditContext,
        outcome: ComplianceEraseOutcome,
    ) -> Result<ComplianceEraseOutcome, ProtocolError> {
        self.record_pre_storage_compliance_outcome(audit, &outcome)
            .await?;
        Ok(outcome)
    }

    pub(in crate::engine) async fn compliance_controller_authorized(
        &self,
        authz: &AuthzContext,
        target: &ComplianceEraseTarget,
    ) -> bool {
        if authz.auth_path() == AuthPath::Delegated {
            return false;
        }
        if authz.auth_path() == AuthPath::System {
            return true;
        }
        let Some(port) = &self.storage.compliance.compliance_admin else {
            return false;
        };
        port.may_perform_compliance_erase(authz, target)
            .await
            .unwrap_or(false)
    }

    async fn compliance_export_controller_authorized(
        &self,
        authz: &AuthzContext,
        target: &ComplianceExportTarget,
    ) -> bool {
        if authz.auth_path() == AuthPath::Delegated {
            return false;
        }
        if authz.auth_path() == AuthPath::System {
            return true;
        }
        let Some(port) = &self.storage.compliance.compliance_admin else {
            return false;
        };
        port.may_perform_compliance_export(authz, target)
            .await
            .unwrap_or(false)
    }

    async fn operator_maintenance_authorized(&self, authz: &AuthzContext) -> bool {
        if authz.auth_path() == AuthPath::Delegated {
            return false;
        }
        if authz.auth_path() == AuthPath::System {
            return true;
        }
        let Some(port) = &self.storage.compliance.compliance_admin else {
            return false;
        };
        port.may_perform_operator_maintenance(authz)
            .await
            .unwrap_or(false)
    }

    /// Owner-agnostic embedding ANN health signals.
    ///
    /// Requires system or compliance/operator-maintenance authority.
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
            .compliance
            .embedding_maintenance
            .embedding_ann_observability(OperatorMaintenanceProof::new())
            .await
            .map_err(|e| ProtocolError::internal(format!("embedding_ann_observability: {e}")))
    }

    /// Sweep orphaned embedding infrastructure rows.
    ///
    /// This is crash-residue maintenance only. Compliance erase remains
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
            .compliance
            .embedding_maintenance
            .sweep_orphan_embedding_rows(OperatorMaintenanceProof::new())
            .await
            .map_err(|e| ProtocolError::internal(format!("sweep_orphan_embedding_rows: {e}")))
    }

    async fn verify_personal_owner_drop(
        &self,
        user_id: UserId,
        drop_event_id: &str,
    ) -> Result<bool, ComplianceEraseRefusal> {
        let Some(port) = &self.storage.compliance.owner_drop_proof else {
            return Err(ComplianceEraseRefusal::DropProofPortUnavailable);
        };
        match port
            .verify_personal_owner_dropped(user_id, drop_event_id)
            .await
        {
            Ok(true) => Ok(true),
            Ok(false) | Err(_) => Err(ComplianceEraseRefusal::PersonalDropNotVerified),
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
    /// owner-scope erase completes in Postgres, then reconcile the durable
    /// `cited_object_purge_pending` audit flag to match reality:
    ///
    /// - no port configured: the storage verb already persisted `false`.
    /// - purge succeeds: clear the durable flag and report `false`.
    /// - purge fails: leave the durable flag set (already `true` from the
    ///   storage verb) and report `true`; retried out-of-band.
    /// - purge succeeds but the clear itself fails: warn and report `true` —
    ///   over-reporting pending is safe, under-reporting is not.
    async fn finalize_owner_erase_with_object_purge(
        &self,
        owner: OwnerRef,
        outcome: ComplianceEraseOutcome,
    ) -> ComplianceEraseOutcome {
        let ComplianceEraseOutcome::Completed {
            operation_id,
            counts,
            ..
        } = outcome
        else {
            return outcome;
        };
        let cited_object_purge_pending = if let Some(port) = self.cited_object_erase() {
            match port.purge_owner_objects(owner).await {
                Ok(purged) => {
                    tracing::debug!(?owner, purged, "owner-scope erase purged cited objects");
                    match self
                        .storage
                        .compliance
                        .compliance_erase
                        .clear_cited_object_purge_pending(operation_id)
                        .await
                    {
                        Ok(()) => false,
                        Err(error) => {
                            tracing::warn!(
                                ?owner,
                                %error,
                                "owner-scope erase purged cited objects but failed to clear \
                                 the durable purge-pending audit flag; the row stays pending \
                                 until an operator retries the clear"
                            );
                            true
                        }
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        ?owner,
                        %error,
                        "owner-scope compliance erase committed but cited-object purge failed; \
                         object-store cleanup must be retried out-of-band"
                    );
                    true
                }
            }
        } else {
            false
        };
        ComplianceEraseOutcome::Completed {
            operation_id,
            counts,
            cited_object_purge_pending,
        }
    }

    /// The front half of every compliance erase: open the audit row, check
    /// compliance-controller authority, and — when the target names a drop
    /// event — verify it before storage can receive a deletion token.
    ///
    /// Both refusals are recorded before storage, against the operation id
    /// the caller is handed back, so an attempt that deleted nothing leaves
    /// the same trail as one that deleted everything. Sharing this is what
    /// makes that true of all four erasures rather than of whichever ones
    /// remembered to do it.
    ///
    /// `drop_event` is `Some` exactly for the two personal targets. A group
    /// owner declares no drop event; its abandonment is what storage rechecks
    /// under lock instead.
    async fn admit_erase(
        &self,
        authz: &AuthzContext,
        target: ComplianceEraseTarget,
        drop_event: Option<(UserId, &str)>,
    ) -> Result<EraseAdmission, ProtocolError> {
        let audit = Self::compliance_audit_context(authz, target.clone());
        let operation_id = audit.operation_id();

        if !self.compliance_controller_authorized(authz, &target).await {
            return self
                .pre_storage_outcome(
                    &audit,
                    ComplianceEraseOutcome::Unauthorized { operation_id },
                )
                .await
                .map(EraseAdmission::Refused);
        }

        if let Some((user_id, drop_event_id)) = drop_event
            && let Err(reason) = self
                .verify_personal_owner_drop(user_id, drop_event_id)
                .await
        {
            return self
                .pre_storage_outcome(
                    &audit,
                    ComplianceEraseOutcome::Refused {
                        operation_id,
                        reason,
                    },
                )
                .await
                .map(EraseAdmission::Refused);
        }

        Ok(EraseAdmission::Admitted(EraseAuthorization::new(audit)))
    }

    /// Refuse and audit an attempted world-owner erase.
    ///
    /// # Errors
    ///
    /// Returns a protocol error when audit recording fails.
    pub async fn erase_world_owner(
        &self,
        authz: &AuthzContext,
    ) -> Result<ComplianceEraseOutcome, ProtocolError> {
        let target = ComplianceEraseTarget::WorldOwner;
        let audit = Self::compliance_audit_context(authz, target);
        let operation_id = audit.operation_id();
        let outcome = self
            .pre_storage_outcome(
                &audit,
                ComplianceEraseOutcome::Refused {
                    operation_id,
                    reason: ComplianceEraseRefusal::WorldOwner,
                },
            )
            .await?;
        Ok(outcome)
    }

    /// Export one owner's compliance bundle.
    ///
    /// Requires compliance-controller authorization. Export is
    /// non-destructive: legal hold and personal-owner drop proof do not gate it.
    ///
    /// # Errors
    ///
    /// Returns forbidden when the caller lacks compliance-controller authority,
    /// or internal when storage export fails.
    pub async fn export_owner_bundle(
        &self,
        authz: &AuthzContext,
        target: ComplianceExportTarget,
    ) -> Result<ComplianceExportBundle, ProtocolError> {
        if !self
            .compliance_export_controller_authorized(authz, &target)
            .await
        {
            return Err(ProtocolError::forbidden(
                "compliance export requires compliance-controller authorization",
            ));
        }

        let auth = ExportAuthorization::new(Self::compliance_export_audit_context(authz, target));
        let sidecars = self.compliance_sidecar_tables();
        self.storage
            .compliance
            .compliance_erase
            .export_owner_bundle(
                &auth,
                &sidecars.fact,
                &sidecars.goal,
                &sidecars.citation_mapping,
                &sidecars.cited_object,
            )
            .await
            .map_err(|e| ProtocolError::internal(format!("export_owner_bundle: {e}")))
    }

    /// Erase an abandoned group owner and all its owned rows.
    ///
    /// Requires `ComplianceAdminPort` approval or [`AuthPath::System`].
    /// Storage rechecks legal hold and abandonment under lock before deleting.
    ///
    /// # Errors
    ///
    /// Returns a protocol error when audit recording or storage execution fails.
    pub async fn erase_abandoned_group_owner(
        &self,
        authz: &AuthzContext,
        group_id: GroupId,
    ) -> Result<ComplianceEraseOutcome, ProtocolError> {
        let auth = match self
            .admit_erase(authz, ComplianceEraseTarget::GroupOwner { group_id }, None)
            .await?
        {
            EraseAdmission::Admitted(auth) => auth,
            EraseAdmission::Refused(outcome) => return Ok(outcome),
        };
        let sidecars = self.compliance_sidecar_tables();
        let object_purge_planned = self.owner_object_purge_planned();
        let outcome = self
            .storage
            .compliance
            .compliance_erase
            .erase_group_owner_if_abandoned(
                &auth,
                group_id,
                object_purge_planned,
                &sidecars.fact,
                &sidecars.goal,
                &sidecars.citation_mapping,
                &sidecars.cited_object,
            )
            .await
            .map_err(|e| ProtocolError::internal(format!("erase_group_owner_if_abandoned: {e}")))?;
        Ok(self
            .finalize_owner_erase_with_object_purge(OwnerRef::Group(group_id), outcome)
            .await)
    }

    /// Erase a dropped personal owner and all its owned rows.
    ///
    /// Requires `ComplianceAdminPort`/system authorization and trusted host
    /// drop proof before storage can receive a deletion token. Storage refuses
    /// with `LegalHoldActive` when the owner hold is active.
    ///
    /// # Errors
    ///
    /// Returns a protocol error when audit recording, drop-proof access, or storage execution fails.
    pub async fn erase_dropped_personal_owner(
        &self,
        authz: &AuthzContext,
        user_id: UserId,
        drop_event_id: String,
    ) -> Result<ComplianceEraseOutcome, ProtocolError> {
        let target = ComplianceEraseTarget::PersonalOwner {
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
        let sidecars = self.compliance_sidecar_tables();
        let object_purge_planned = self.owner_object_purge_planned();
        let outcome = self
            .storage
            .compliance
            .compliance_erase
            .erase_personal_owner_if_drop_verified(
                &auth,
                user_id,
                object_purge_planned,
                &sidecars.fact,
                &sidecars.goal,
                &sidecars.citation_mapping,
                &sidecars.cited_object,
            )
            .await
            .map_err(|e| {
                ProtocolError::internal(format!("erase_personal_owner_if_drop_verified: {e}"))
            })?;
        Ok(self
            .finalize_owner_erase_with_object_purge(OwnerRef::Personal(user_id), outcome)
            .await)
    }

    /// Erase one source scope for an abandoned group owner.
    /// Storage rechecks legal hold and abandonment under lock before deleting.
    ///
    /// # Errors
    ///
    /// Returns a protocol error when audit recording or storage execution fails.
    pub async fn erase_abandoned_group_source_scope(
        &self,
        authz: &AuthzContext,
        group_id: GroupId,
        source_id: SourceId,
    ) -> Result<ComplianceEraseOutcome, ProtocolError> {
        let target = ComplianceEraseTarget::GroupSourceScope {
            group_id,
            source_id: source_id.clone(),
        };
        let auth = match self.admit_erase(authz, target, None).await? {
            EraseAdmission::Admitted(auth) => auth,
            EraseAdmission::Refused(outcome) => return Ok(outcome),
        };
        let sidecars = self.compliance_sidecar_tables();
        // Source-scope blob purge is deferred: a source scope is a partial
        // owner, and the object store keys purely by owner, so a prefix-delete
        // would over-delete the owner's other sources. Only owner-scope erase
        // purges the object store.
        self.storage
            .compliance
            .compliance_erase
            .erase_group_source_scope_if_owner_abandoned(
                &auth,
                group_id,
                &source_id,
                &sidecars.fact,
                &sidecars.goal,
                &sidecars.citation_mapping,
                &sidecars.cited_object,
            )
            .await
            .map_err(|e| {
                ProtocolError::internal(format!("erase_group_source_scope_if_owner_abandoned: {e}"))
            })
    }

    /// Erase one source scope for a dropped personal owner.
    /// Storage refuses with `LegalHoldActive` when the owner hold is active.
    ///
    /// # Errors
    ///
    /// Returns a protocol error when audit recording, drop-proof access, or storage execution fails.
    pub async fn erase_dropped_personal_source_scope(
        &self,
        authz: &AuthzContext,
        user_id: UserId,
        source_id: SourceId,
        drop_event_id: String,
    ) -> Result<ComplianceEraseOutcome, ProtocolError> {
        let target = ComplianceEraseTarget::PersonalSourceScope {
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
        let sidecars = self.compliance_sidecar_tables();
        // Source-scope blob purge is deferred (see
        // `erase_abandoned_group_source_scope`): prefix-delete keys by owner,
        // so it would over-delete the owner's other sources.
        self.storage
            .compliance
            .compliance_erase
            .erase_personal_source_scope_if_drop_verified(
                &auth,
                user_id,
                &source_id,
                &sidecars.fact,
                &sidecars.goal,
                &sidecars.citation_mapping,
                &sidecars.cited_object,
            )
            .await
            .map_err(|e| {
                ProtocolError::internal(format!(
                    "erase_personal_source_scope_if_drop_verified: {e}"
                ))
            })
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
    use crate::compliance::{
        ComplianceAuditContext, ComplianceEraseCounts, ComplianceEraseOutcome,
        ComplianceEraseRefusal, ComplianceExportBundle, EraseAuthorization, ExportAuthorization,
    };
    use crate::storage::StorageError;
    use crate::storage_ports::{
        CitedObjectErasePort, ComplianceAdminPort, ComplianceErasePort, OwnerDropProofPort,
        StoragePorts,
    };
    use crate::{GroupId, OwnerRef, SourceId, UserId};

    #[derive(Debug, Default)]
    struct PermissiveAdmin {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl ComplianceAdminPort for PermissiveAdmin {
        async fn may_perform_compliance_erase(
            &self,
            _authz: &AuthzContext,
            _target: &crate::compliance::ComplianceEraseTarget,
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

    /// `ComplianceErasePort` whose erase verbs return a fixed outcome and whose
    /// `record_compliance_outcome` succeeds (so the refused audit path can
    /// run). Also records every `clear_cited_object_purge_pending` call so
    /// tests can assert the durable flag was (or was not) cleared.
    #[derive(Debug)]
    struct FixedOutcomeErase {
        outcome: ComplianceEraseOutcome,
        clear_calls: Mutex<Vec<uuid::Uuid>>,
        fail_clear: bool,
        /// Bumped by every erase verb, so a test can assert an attempt was
        /// turned away before storage rather than merely reporting a refusal.
        erase_calls: AtomicUsize,
        /// Bumped by every audit write, so a test can assert the refusal it
        /// was handed is also the one that got recorded.
        recorded: AtomicUsize,
    }

    impl FixedOutcomeErase {
        fn completed() -> Self {
            Self::completed_with_clear_outcome(true)
        }

        /// Same fixed `Completed` outcome, but
        /// `clear_cited_object_purge_pending` fails every call — exercises
        /// the "purge succeeded but the durable clear itself failed" corner.
        fn completed_with_failing_clear() -> Self {
            Self::completed_with_clear_outcome(false)
        }

        fn completed_with_clear_outcome(clear_succeeds: bool) -> Self {
            Self {
                outcome: ComplianceEraseOutcome::Completed {
                    operation_id: uuid::Uuid::now_v7(),
                    counts: ComplianceEraseCounts::default(),
                    cited_object_purge_pending: false,
                },
                clear_calls: Mutex::new(Vec::new()),
                fail_clear: !clear_succeeds,
                erase_calls: AtomicUsize::new(0),
                recorded: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl ComplianceErasePort for FixedOutcomeErase {
        async fn record_compliance_outcome(
            &self,
            _audit: &ComplianceAuditContext,
            _outcome: &ComplianceEraseOutcome,
        ) -> Result<(), StorageError> {
            self.recorded.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn erase_group_owner_if_abandoned(
            &self,
            _auth: &EraseAuthorization,
            _group_id: GroupId,
            _object_purge_planned: bool,
            _fact: &[String],
            _goal: &[String],
            _citation_mapping: &[String],
            _cited_object: &[String],
        ) -> Result<ComplianceEraseOutcome, StorageError> {
            self.erase_calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.outcome.clone())
        }

        async fn erase_personal_owner_if_drop_verified(
            &self,
            _auth: &EraseAuthorization,
            _user_id: UserId,
            _object_purge_planned: bool,
            _fact: &[String],
            _goal: &[String],
            _citation_mapping: &[String],
            _cited_object: &[String],
        ) -> Result<ComplianceEraseOutcome, StorageError> {
            self.erase_calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.outcome.clone())
        }

        async fn erase_group_source_scope_if_owner_abandoned(
            &self,
            _auth: &EraseAuthorization,
            _group_id: GroupId,
            _source_id: &SourceId,
            _fact: &[String],
            _goal: &[String],
            _citation_mapping: &[String],
            _cited_object: &[String],
        ) -> Result<ComplianceEraseOutcome, StorageError> {
            self.erase_calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.outcome.clone())
        }

        async fn erase_personal_source_scope_if_drop_verified(
            &self,
            _auth: &EraseAuthorization,
            _user_id: UserId,
            _source_id: &SourceId,
            _fact: &[String],
            _goal: &[String],
            _citation_mapping: &[String],
            _cited_object: &[String],
        ) -> Result<ComplianceEraseOutcome, StorageError> {
            self.erase_calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.outcome.clone())
        }

        async fn export_owner_bundle(
            &self,
            _auth: &ExportAuthorization,
            _fact: &[String],
            _goal: &[String],
            _citation_mapping: &[String],
            _cited_object: &[String],
        ) -> Result<ComplianceExportBundle, StorageError> {
            Err(StorageError::Internal("export not used in test".into()))
        }

        async fn clear_cited_object_purge_pending(
            &self,
            operation_id: uuid::Uuid,
        ) -> Result<(), StorageError> {
            self.clear_calls.lock().unwrap().push(operation_id);
            if self.fail_clear {
                Err(StorageError::Internal(
                    "clear_cited_object_purge_pending failed in test".into(),
                ))
            } else {
                Ok(())
            }
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
        let storage = StoragePorts::rejecting_with_compliance_erase(erase, drop_proof);
        Engine::compose_or_panic_for_tests(storage, |_| {}).with_cited_object_erase(purge)
    }

    #[tokio::test]
    async fn raw_delegated_maintenance_is_denied_before_admin_or_storage() {
        let admin = Arc::new(PermissiveAdmin::default());
        let storage = StoragePorts::rejecting_with_compliance_admin(admin.clone());
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
            .erase_abandoned_group_owner(&system_authz(), group)
            .await
            .expect("erase returns an outcome");

        let ComplianceEraseOutcome::Completed {
            operation_id,
            cited_object_purge_pending,
            ..
        } = outcome
        else {
            panic!("expected completed erase, got {outcome:?}");
        };
        assert!(
            !cited_object_purge_pending,
            "a successful purge must clear the pending flag"
        );
        assert_eq!(
            purge.purged.lock().unwrap().as_slice(),
            &[OwnerRef::Group(group)]
        );
        assert_eq!(
            erase.clear_calls.lock().unwrap().as_slice(),
            &[operation_id],
            "purge success must clear the durable audit flag exactly once"
        );
    }

    #[tokio::test]
    async fn completed_personal_owner_erase_purges_once_with_owner() {
        let user = UserId::new(uuid::Uuid::now_v7());
        let purge = Arc::new(RecordingPurge::default());
        let erase = Arc::new(FixedOutcomeErase::completed());
        let engine = engine_with(erase.clone(), Some(Arc::new(DropVerified)), purge.clone());

        let outcome = engine
            .erase_dropped_personal_owner(&system_authz(), user, "drop-1".into())
            .await
            .expect("erase returns an outcome");

        let ComplianceEraseOutcome::Completed {
            operation_id,
            cited_object_purge_pending,
            ..
        } = outcome
        else {
            panic!("expected completed erase, got {outcome:?}");
        };
        assert!(
            !cited_object_purge_pending,
            "a successful purge must clear the pending flag"
        );
        assert_eq!(
            purge.purged.lock().unwrap().as_slice(),
            &[OwnerRef::Personal(user)]
        );
        assert_eq!(
            erase.clear_calls.lock().unwrap().as_slice(),
            &[operation_id],
            "purge success must clear the durable audit flag exactly once"
        );
    }

    #[tokio::test]
    async fn refused_world_owner_erase_does_not_purge() {
        let purge = Arc::new(RecordingPurge::default());
        let engine = engine_with(
            Arc::new(FixedOutcomeErase::completed()),
            None,
            purge.clone(),
        );

        let outcome = engine
            .erase_world_owner(&system_authz())
            .await
            .expect("world erase records a refusal");

        assert!(matches!(
            outcome,
            ComplianceEraseOutcome::Refused {
                reason: ComplianceEraseRefusal::WorldOwner,
                ..
            }
        ));
        assert!(purge.purged.lock().unwrap().is_empty());
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
            .erase_abandoned_group_source_scope(&system_authz(), group, source)
            .await
            .expect("source-scope erase completes");

        assert!(matches!(outcome, ComplianceEraseOutcome::Completed { .. }));
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
            .erase_abandoned_group_owner(&system_authz(), group)
            .await
            .expect("best-effort purge failure must not fail the erase");

        assert!(
            matches!(
                outcome,
                ComplianceEraseOutcome::Completed {
                    cited_object_purge_pending: true,
                    ..
                }
            ),
            "purge failure must surface cited_object_purge_pending on Completed"
        );
        // Attempted exactly once even though it errored.
        assert_eq!(purge.purged.lock().unwrap().len(), 1);
        assert!(
            erase.clear_calls.lock().unwrap().is_empty(),
            "a failed purge must leave the durable pending flag untouched (already true)"
        );
    }

    #[tokio::test]
    async fn clear_failure_after_successful_purge_still_reports_pending() {
        let group = GroupId::new(uuid::Uuid::now_v7());
        let purge = Arc::new(RecordingPurge::default());
        let erase = Arc::new(FixedOutcomeErase::completed_with_failing_clear());
        let engine = engine_with(erase.clone(), None, purge.clone());

        let outcome = engine
            .erase_abandoned_group_owner(&system_authz(), group)
            .await
            .expect("a failed clear must not fail the erase itself");

        assert!(
            matches!(
                outcome,
                ComplianceEraseOutcome::Completed {
                    cited_object_purge_pending: true,
                    ..
                }
            ),
            "a failed clear must over-report pending rather than silently under-report"
        );
        assert_eq!(purge.purged.lock().unwrap().len(), 1);
        assert_eq!(
            erase.clear_calls.lock().unwrap().len(),
            1,
            "the clear must still be attempted exactly once"
        );
    }

    /// Drive all four erase entry points against one engine, so a gate that
    /// only some of them apply cannot pass.
    async fn attempt_every_scope(
        engine: &Engine,
        authz: &AuthzContext,
    ) -> Vec<ComplianceEraseOutcome> {
        let group = GroupId::new(uuid::Uuid::now_v7());
        let user = UserId::new(uuid::Uuid::now_v7());
        let source = SourceId::new("source/a");
        vec![
            engine
                .erase_abandoned_group_owner(authz, group)
                .await
                .expect("group owner erase returns an outcome"),
            engine
                .erase_dropped_personal_owner(authz, user, "drop-1".into())
                .await
                .expect("personal owner erase returns an outcome"),
            engine
                .erase_abandoned_group_source_scope(authz, group, source.clone())
                .await
                .expect("group source scope erase returns an outcome"),
            engine
                .erase_dropped_personal_source_scope(authz, user, source, "drop-1".into())
                .await
                .expect("personal source scope erase returns an outcome"),
        ]
    }

    #[tokio::test]
    async fn every_scope_audits_an_unauthorized_attempt_before_storage() {
        // No `ComplianceAdminPort` and a caller who is not `System`: nothing
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
                matches!(outcome, ComplianceEraseOutcome::Unauthorized { .. }),
                "expected an unauthorized outcome, got {outcome:?}"
            );
        }
        assert_eq!(
            erase.erase_calls.load(Ordering::SeqCst),
            0,
            "an unauthorized attempt must be turned away before storage"
        );
        assert_eq!(
            erase.recorded.load(Ordering::SeqCst),
            outcomes.len(),
            "every unauthorized attempt must leave an audit row"
        );
    }

    #[tokio::test]
    async fn the_personal_scopes_refuse_before_storage_without_a_verified_drop() {
        // Authorized, but the drop the request names is not one the host will
        // confirm. Both personal scopes must stop; both group scopes, which
        // name no drop event, must still reach storage.
        for (drop_proof, expected) in [
            (
                Some(Arc::new(DropUnverified)),
                ComplianceEraseRefusal::PersonalDropNotVerified,
            ),
            (None, ComplianceEraseRefusal::DropProofPortUnavailable),
        ] {
            let erase = Arc::new(FixedOutcomeErase::completed());
            let drop_proof = drop_proof.map(|d| d as Arc<dyn OwnerDropProofPort>);
            let storage = StoragePorts::rejecting_with_compliance_erase(erase.clone(), drop_proof);
            let engine = Engine::compose_or_panic_for_tests(storage, |_| {});

            let outcomes = attempt_every_scope(&engine, &system_authz()).await;

            for outcome in [&outcomes[1], &outcomes[3]] {
                let ComplianceEraseOutcome::Refused { reason, .. } = outcome else {
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
