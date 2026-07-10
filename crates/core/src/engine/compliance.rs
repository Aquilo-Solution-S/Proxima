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

struct ComplianceSidecarTables {
    fact: Vec<String>,
    goal: Vec<String>,
    edge: Vec<String>,
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
            edge: sidecar_tables(self.registry.schemas(), PayloadKind::Edge),
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

    /// Reclaim cited-object payloads from the host-wired object store after an
    /// owner-scope erase completes in Postgres. Returns the outcome with
    /// [`ComplianceEraseOutcome::Completed::cited_object_purge_pending`] set
    /// when purge fails so operators can retry out-of-band.
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
        let Some(port) = self.cited_object_erase() else {
            return ComplianceEraseOutcome::Completed {
                operation_id,
                counts,
                cited_object_purge_pending: false,
            };
        };
        match port.purge_owner_objects(owner).await {
            Ok(purged) => {
                tracing::debug!(?owner, purged, "owner-scope erase purged cited objects");
                ComplianceEraseOutcome::Completed {
                    operation_id,
                    counts,
                    cited_object_purge_pending: false,
                }
            }
            Err(error) => {
                tracing::warn!(
                    ?owner,
                    %error,
                    "owner-scope compliance erase committed but cited-object purge failed; \
                     object-store cleanup must be retried out-of-band"
                );
                ComplianceEraseOutcome::Completed {
                    operation_id,
                    counts,
                    cited_object_purge_pending: true,
                }
            }
        }
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
                &sidecars.edge,
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
        let target = ComplianceEraseTarget::GroupOwner { group_id };
        let audit = Self::compliance_audit_context(authz, target.clone());
        let operation_id = audit.operation_id();

        if !self.compliance_controller_authorized(authz, &target).await {
            return self
                .pre_storage_outcome(
                    &audit,
                    ComplianceEraseOutcome::Unauthorized { operation_id },
                )
                .await;
        }

        let auth = EraseAuthorization::new(audit);
        let sidecars = self.compliance_sidecar_tables();
        let outcome = self
            .storage
            .compliance
            .compliance_erase
            .erase_group_owner_if_abandoned(
                &auth,
                group_id,
                &sidecars.fact,
                &sidecars.goal,
                &sidecars.edge,
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
        let audit = Self::compliance_audit_context(authz, target.clone());
        let operation_id = audit.operation_id();

        if !self.compliance_controller_authorized(authz, &target).await {
            return self
                .pre_storage_outcome(
                    &audit,
                    ComplianceEraseOutcome::Unauthorized { operation_id },
                )
                .await;
        }

        if let Err(reason) = self
            .verify_personal_owner_drop(user_id, &drop_event_id)
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
                .await;
        }

        let auth = EraseAuthorization::new(audit);
        let sidecars = self.compliance_sidecar_tables();
        let outcome = self
            .storage
            .compliance
            .compliance_erase
            .erase_personal_owner_if_drop_verified(
                &auth,
                user_id,
                &sidecars.fact,
                &sidecars.goal,
                &sidecars.edge,
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
        let audit = Self::compliance_audit_context(authz, target.clone());
        let operation_id = audit.operation_id();

        if !self.compliance_controller_authorized(authz, &target).await {
            return self
                .pre_storage_outcome(
                    &audit,
                    ComplianceEraseOutcome::Unauthorized { operation_id },
                )
                .await;
        }

        let auth = EraseAuthorization::new(audit);
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
                &sidecars.edge,
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
        let audit = Self::compliance_audit_context(authz, target.clone());
        let operation_id = audit.operation_id();

        if !self.compliance_controller_authorized(authz, &target).await {
            return self
                .pre_storage_outcome(
                    &audit,
                    ComplianceEraseOutcome::Unauthorized { operation_id },
                )
                .await;
        }

        if let Err(reason) = self
            .verify_personal_owner_drop(user_id, &drop_event_id)
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
                .await;
        }

        let auth = EraseAuthorization::new(audit);
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
                &sidecars.edge,
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
        CitedObjectErasePort, ComplianceErasePort, OwnerDropProofPort, StoragePorts,
    };
    use crate::{GroupId, OwnerRef, SourceId, UserId};

    /// `ComplianceErasePort` whose erase verbs return a fixed outcome and whose
    /// `record_compliance_outcome` succeeds (so the refused audit path can run).
    #[derive(Debug)]
    struct FixedOutcomeErase {
        outcome: ComplianceEraseOutcome,
    }

    impl FixedOutcomeErase {
        fn completed() -> Self {
            Self {
                outcome: ComplianceEraseOutcome::Completed {
                    operation_id: uuid::Uuid::now_v7(),
                    counts: ComplianceEraseCounts::default(),
                    cited_object_purge_pending: false,
                },
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
            Ok(())
        }

        async fn erase_group_owner_if_abandoned(
            &self,
            _auth: &EraseAuthorization,
            _group_id: GroupId,
            _fact: &[String],
            _goal: &[String],
            _edge: &[String],
            _citation_mapping: &[String],
            _cited_object: &[String],
        ) -> Result<ComplianceEraseOutcome, StorageError> {
            Ok(self.outcome.clone())
        }

        async fn erase_personal_owner_if_drop_verified(
            &self,
            _auth: &EraseAuthorization,
            _user_id: UserId,
            _fact: &[String],
            _goal: &[String],
            _edge: &[String],
            _citation_mapping: &[String],
            _cited_object: &[String],
        ) -> Result<ComplianceEraseOutcome, StorageError> {
            Ok(self.outcome.clone())
        }

        async fn erase_group_source_scope_if_owner_abandoned(
            &self,
            _auth: &EraseAuthorization,
            _group_id: GroupId,
            _source_id: &SourceId,
            _fact: &[String],
            _goal: &[String],
            _edge: &[String],
            _citation_mapping: &[String],
            _cited_object: &[String],
        ) -> Result<ComplianceEraseOutcome, StorageError> {
            Ok(self.outcome.clone())
        }

        async fn erase_personal_source_scope_if_drop_verified(
            &self,
            _auth: &EraseAuthorization,
            _user_id: UserId,
            _source_id: &SourceId,
            _fact: &[String],
            _goal: &[String],
            _edge: &[String],
            _citation_mapping: &[String],
            _cited_object: &[String],
        ) -> Result<ComplianceEraseOutcome, StorageError> {
            Ok(self.outcome.clone())
        }

        async fn export_owner_bundle(
            &self,
            _auth: &ExportAuthorization,
            _fact: &[String],
            _goal: &[String],
            _edge: &[String],
            _citation_mapping: &[String],
            _cited_object: &[String],
        ) -> Result<ComplianceExportBundle, StorageError> {
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
    async fn completed_group_owner_erase_purges_once_with_owner() {
        let group = GroupId::new(uuid::Uuid::now_v7());
        let purge = Arc::new(RecordingPurge::default());
        let engine = engine_with(
            Arc::new(FixedOutcomeErase::completed()),
            None,
            purge.clone(),
        );

        let outcome = engine
            .erase_abandoned_group_owner(&system_authz(), group)
            .await
            .expect("erase returns an outcome");

        assert!(matches!(outcome, ComplianceEraseOutcome::Completed { .. }));
        assert_eq!(
            purge.purged.lock().unwrap().as_slice(),
            &[OwnerRef::Group(group)]
        );
    }

    #[tokio::test]
    async fn completed_personal_owner_erase_purges_once_with_owner() {
        let user = UserId::new(uuid::Uuid::now_v7());
        let purge = Arc::new(RecordingPurge::default());
        let engine = engine_with(
            Arc::new(FixedOutcomeErase::completed()),
            Some(Arc::new(DropVerified)),
            purge.clone(),
        );

        let outcome = engine
            .erase_dropped_personal_owner(&system_authz(), user, "drop-1".into())
            .await
            .expect("erase returns an outcome");

        assert!(matches!(outcome, ComplianceEraseOutcome::Completed { .. }));
        assert_eq!(
            purge.purged.lock().unwrap().as_slice(),
            &[OwnerRef::Personal(user)]
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
        let engine = engine_with(
            Arc::new(FixedOutcomeErase::completed()),
            None,
            purge.clone(),
        );

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
    }
}
