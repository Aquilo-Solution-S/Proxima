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
use crate::verbs::schema::PayloadKind;
use crate::{GroupId, SourceId, UserId};

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
        self.pre_storage_outcome(
            &audit,
            ComplianceEraseOutcome::Refused {
                operation_id,
                reason: ComplianceEraseRefusal::WorldOwner,
            },
        )
        .await
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
        self.storage
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
            .map_err(|e| ProtocolError::internal(format!("erase_group_owner_if_abandoned: {e}")))
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
        self.storage
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
            })
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
