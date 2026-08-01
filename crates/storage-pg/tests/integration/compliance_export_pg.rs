//! Compliance export integration contracts.

use std::sync::Arc;

use crate::common::{
    drop_db, engine_with_registry, fresh_pg, owner_write_permit, storage_ports_with_compliance,
};
use proxima_core::access::{AccessError, Role};
use proxima_core::storage_ports::{ComplianceAdminPort, FactIngestPort, SourceCursorPort};
use proxima_core::verbs::fact_ingest::{FactReceiptDraft, FactWriteCommand};
use proxima_core::{
    AccessKind, AuthPath, AuthzContext, ComplianceEraseOutcome, ComplianceEraseRefusal,
    ComplianceEraseTarget, ComplianceExportTarget, Cursor, FactPayload, FlavorRegistry, GroupId,
    OwnerRef, PayloadKeyBuilder, SchemaId, SchemaVersion, SourceBatchId, SourceId, UserId,
};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

#[derive(Debug)]
struct AllowComplianceAdmin;

#[async_trait::async_trait]
impl ComplianceAdminPort for AllowComplianceAdmin {
    async fn may_perform_compliance_erase(
        &self,
        _authz: &AuthzContext,
        _target: &ComplianceEraseTarget,
    ) -> Result<bool, AccessError> {
        Ok(true)
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ExportFactPayload {
    label: String,
}

impl FactPayload for ExportFactPayload {
    const SCHEMA_ID: &'static str = "test/compliance-export-fact-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn receipt_key(&self) -> Vec<u8> {
        let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
        key.field_str("label", &self.label);
        key.finish()
    }

    fn render(&self) -> String {
        self.label.clone()
    }

    fn sidecar_table() -> Option<&'static str> {
        Some("public.compliance_export_fact_sidecar")
    }
}

fn export_registry() -> proxima_core::verbs::schema::FlavorRegistryFrozen {
    let mut registry = FlavorRegistry::new();
    registry
        .try_add_fact_schema::<ExportFactPayload>()
        .expect("test schema registers");
    registry.freeze_or_panic_for_tests()
}

fn compliance_engine(pg: &PgStorage) -> proxima_core::Engine {
    proxima_core::Engine::new(export_registry()).with_storage_ports(storage_ports_with_compliance(
        pg,
        Arc::new(AllowComplianceAdmin),
    ))
}

fn engine_without_compliance_admin(pg: &PgStorage) -> proxima_core::Engine {
    engine_with_registry(pg, export_registry())
}

fn admin_authz_for(owner: OwnerRef) -> AuthzContext {
    AuthzContext::for_subject_with_role(
        UserId::new(Uuid::now_v7()),
        [(owner, Role::admin())],
        AuthPath::HostBearer,
    )
}

fn fact_command(label: &str) -> FactWriteCommand {
    let now = time::OffsetDateTime::now_utc();
    FactWriteCommand {
        schema_id: SchemaId::new(ExportFactPayload::SCHEMA_ID.into()),
        schema_version: SchemaVersion::new(ExportFactPayload::SCHEMA_VERSION),
        payload: serde_json::to_vec(&ExportFactPayload {
            label: label.to_string(),
        })
        .expect("serialize test payload"),
        rendered_text: Some(label.to_string()),
        lexical_language: None,
        receipt: Some(FactReceiptDraft {
            source_id: SourceId::new("test/compliance-export"),
            source_batch_id: SourceBatchId::new(Uuid::now_v7()),
            observed_at: now,
            occurred_at: now,
        }),
        citation: None,
        derived_from: Vec::new(),
    }
}

async fn create_export_sidecar_table(pg: &PgStorage) -> Result<(), sqlx::Error> {
    sqlx::query(
        "CREATE TABLE public.compliance_export_fact_sidecar(
            memory_id uuid PRIMARY KEY,
            label text NOT NULL
        )",
    )
    .execute(pg.pool_for_tests())
    .await?;
    Ok(())
}

async fn seed_fact_with_sidecar(
    pg: &PgStorage,
    owner: OwnerRef,
    label: &str,
) -> Result<proxima_core::FactIngestOutcome, Box<dyn std::error::Error>> {
    let permit = owner_write_permit(&owner, AccessKind::Fact).await?;
    let outcome = pg
        .ingest_fact_atomic(&permit, &fact_command(label), None)
        .await?;
    sqlx::query(
        "INSERT INTO public.compliance_export_fact_sidecar(memory_id, label)
         VALUES ($1, $2)",
    )
    .bind(outcome.memory_id.into_inner())
    .bind(label)
    .execute(pg.pool_for_tests())
    .await?;
    Ok(outcome)
}

async fn attach_uploaded_blob_citation(
    pg: &PgStorage,
    owner: OwnerRef,
    memory_id: Uuid,
) -> Result<(), sqlx::Error> {
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(&owner);
    let cited_object_id = Uuid::now_v7();
    let mapping_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxima_core.cited_objects(
            cited_object_id, schema_id, owner_kind, owner_id, content_hash)
         VALUES ($1, 'core/uploaded-blob-v1', $2, $3, $4)",
    )
    .bind(cited_object_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind([7_u8; 32].as_slice())
    .execute(pg.pool_for_tests())
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.cited_uploaded_blob_v1(
            cited_object_id, bucket, object_key, sha256, byte_len, mime, filename, etag)
         VALUES ($1, 'export-bucket', 'objects/exported', $2, 7, 'text/plain', 'export.txt', 'etag-export')",
    )
    .bind(cited_object_id)
    .bind([8_u8; 32].as_slice())
    .execute(pg.pool_for_tests())
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.citation_mappings(
            citation_mapping_id, schema_id, memory_id, cited_object_id, owner_kind, owner_id)
         VALUES ($1, 'test/export-citation', $2, $3, $4, $5)",
    )
    .bind(mapping_id)
    .bind(memory_id)
    .bind(cited_object_id)
    .bind(owner_kind)
    .bind(owner_id)
    .execute(pg.pool_for_tests())
    .await?;
    sqlx::query(
        "UPDATE proxima_core.memories
            SET citation_mapping_id = $1
          WHERE memory_id = $2",
    )
    .bind(mapping_id)
    .bind(memory_id)
    .execute(pg.pool_for_tests())
    .await?;
    Ok(())
}

#[tokio::test]
async fn owner_export_bundle_is_gated_deterministic_and_legal_hold_readable()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;
    create_export_sidecar_table(&pg).await?;

    let owner = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
    let other_owner = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
    let outcome = seed_fact_with_sidecar(&pg, owner, "included-export-fact").await?;
    seed_fact_with_sidecar(&pg, other_owner, "excluded-export-fact").await?;
    attach_uploaded_blob_citation(&pg, owner, outcome.memory_id.into_inner()).await?;

    let permit = owner_write_permit(&owner, AccessKind::Fact).await?;
    pg.store_source_cursor(
        &permit,
        "test/compliance-export",
        &Cursor::from_bytes(b"cursor-v1".to_vec()),
    )
    .await?;

    let engine = compliance_engine(&pg);
    let authz = admin_authz_for(owner);
    engine.set_legal_hold(&authz, &owner).await?;
    let erase = engine
        .erase_abandoned_group_owner(
            &authz,
            match owner {
                OwnerRef::Group(group) => group,
                _ => unreachable!("test owner is a group"),
            },
        )
        .await?;
    assert!(matches!(
        erase,
        ComplianceEraseOutcome::Refused {
            reason: ComplianceEraseRefusal::LegalHoldActive,
            ..
        }
    ));

    let target = ComplianceExportTarget::GroupOwner {
        group_id: match owner {
            OwnerRef::Group(group) => group,
            _ => unreachable!("test owner is a group"),
        },
    };
    let first = engine.export_owner_bundle(&authz, target.clone()).await?;
    let second = engine.export_owner_bundle(&authz, target.clone()).await?;

    assert_eq!(first.counts.memories, 1);
    assert_eq!(first.counts.receipts, 1);
    assert_eq!(first.counts.source_batches, 1);
    assert_eq!(first.counts.citations, 1);
    assert_eq!(first.counts.cited_objects, 1);
    assert_eq!(first.counts.source_cursors, 1);
    assert!(first.counts.sidecar_rows >= 2);
    assert_eq!(first.counts.compliance_audit_rows, 1);
    assert!(
        first
            .memories
            .iter()
            .any(|row| row["text"] == "included-export-fact")
    );
    assert!(
        !first
            .memories
            .iter()
            .any(|row| row["text"] == "excluded-export-fact")
    );
    assert!(first.sidecars.iter().any(|sidecar| {
        sidecar.table == "public.compliance_export_fact_sidecar"
            && sidecar
                .rows
                .iter()
                .any(|row| row["label"] == "included-export-fact")
    }));
    assert!(first.sidecars.iter().any(|sidecar| {
        sidecar.table == "proxima_core.cited_uploaded_blob_v1"
            && sidecar
                .rows
                .iter()
                .any(|row| row["object_key"] == "objects/exported")
    }));
    assert!(
        first
            .source_cursors
            .iter()
            .any(|row| row["source"] == "test/compliance-export")
    );
    assert_eq!(first.memories, second.memories);
    assert_eq!(first.source_cursors, second.source_cursors);
    assert_eq!(first.sidecars, second.sidecars);
    assert!(
        !first.canonical_json_bytes()?.is_empty(),
        "canonical serialization should produce bytes"
    );

    let unauthorized = engine_without_compliance_admin(&pg)
        .export_owner_bundle(&authz, target)
        .await
        .expect_err("export without compliance admin port must fail closed");
    assert_eq!(unauthorized.code, proxima_core::ErrorCode::Forbidden);

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}
