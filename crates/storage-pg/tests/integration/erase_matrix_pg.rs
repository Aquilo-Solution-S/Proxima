//! Delete matrix coverage for compliance sidecar families.

use std::sync::Arc;

use crate::common::{drop_db, fresh_pg, owner_write_permit};
use proxima_core::access::{AccessError, Role};
use proxima_core::storage_ports::{ComplianceAdminPort, FactIngestPort, StoragePorts};
use proxima_core::verbs::fact_ingest::{FactReceiptDraft, FactWriteCommand};
use proxima_core::{
    AccessKind, AuthPath, AuthzContext, ComplianceEraseOutcome, ComplianceEraseTarget,
    FlavorRegistry, GroupId, OwnerRef, SchemaId, SchemaVersion, SourceBatchId, SourceId, UserId,
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

fn compliance_engine(pg: &PgStorage) -> proxima_core::Engine {
    let pg = Arc::new(pg.clone());
    let ports = StoragePorts::builder()
        .fact_ingest(pg.clone())
        .mcp_call_write(pg.clone())
        .mcp_call_read(pg.clone())
        .memory_authoring(pg.clone())
        .memory_read(pg.clone())
        .memory_inspect(pg.clone())
        .embedding_text(pg.clone())
        .embedding_write(pg.clone())
        .embedding_job(pg.clone())
        .embedding_maintenance(pg.clone())
        .goal_write(pg.clone())
        .goal_read(pg.clone())
        .goal_wake_candidate(pg.clone())
        .change_event(pg.clone())
        .edge_read(pg.clone())
        .citation(pg.clone())
        .owner_access_read(pg.clone())
        .owner_membership_admin(pg.clone())
        .owner_transfer(pg.clone())
        .source_batch(pg.clone())
        .source_cursor(pg.clone())
        .fact_retention(pg.clone())
        .compliance_erase(pg.clone())
        .compliance_admin(Arc::new(AllowComplianceAdmin))
        .registry_projection(pg)
        .build();
    proxima_core::Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests())
        .with_storage_ports(ports)
}

fn admin_authz_for(owner: OwnerRef) -> AuthzContext {
    AuthzContext::for_subject_with_role(
        UserId::new(Uuid::now_v7()),
        [(owner, Role::admin())],
        AuthPath::HostBearer,
    )
}

fn fact_command() -> FactWriteCommand {
    let now = time::OffsetDateTime::now_utc();
    FactWriteCommand {
        schema_id: SchemaId::new("test/erase-matrix-fact".into()),
        schema_version: SchemaVersion::new(1),
        payload: b"erase matrix fact".to_vec(),
        rendered_text: Some("erase matrix fact".into()),
        lexical_language: None,
        receipt: Some(FactReceiptDraft {
            source_id: SourceId::new("test/erase-matrix"),
            source_batch_id: SourceBatchId::new(Uuid::now_v7()),
            observed_at: now,
            occurred_at: now,
        }),
        citation: None,
        derived_from: Vec::new(),
    }
}

async fn seed_fact(pg: &PgStorage, owner: OwnerRef) -> Result<Uuid, Box<dyn std::error::Error>> {
    let permit = owner_write_permit(&owner, AccessKind::Fact).await?;
    Ok(pg
        .ingest_fact_atomic(&permit, &fact_command(), None)
        .await?
        .memory_id
        .into_inner())
}

async fn seed_agent_derivation(
    pg: &PgStorage,
    owner: OwnerRef,
    source_memory_id: Uuid,
) -> Result<Uuid, sqlx::Error> {
    let abstraction_id = Uuid::now_v7();
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(&owner);
    sqlx::query(
        "INSERT INTO proxima_core.memories(
            memory_id, owner_kind, owner_id, schema_id, schema_version,
            kind, text, operator_kind, operator_id, input_contract_id,
            model_id, prompt_version)
         VALUES (
            $1, $2, $3, 'core/agent-derivation-v1', 1,
            'Abstraction', 'erase matrix abstraction',
            'AtoA', $4, $5, 'matrix-model', 'matrix-v1')",
    )
    .bind(abstraction_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(Uuid::now_v7())
    .bind(Uuid::now_v7())
    .execute(pg.pool_for_tests())
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.agent_derivation_v1(
            memory_id, title, body, tags, idempotency_key, source_memory_ids,
            model_id, client_name, client_version)
         VALUES ($1, 'erase matrix', 'delete abstraction sidecar',
                 ARRAY['matrix'], 'erase-matrix', ARRAY[$2]::uuid[],
                 'matrix-model', 'test', '1')",
    )
    .bind(abstraction_id)
    .bind(source_memory_id)
    .execute(pg.pool_for_tests())
    .await?;
    Ok(abstraction_id)
}

#[tokio::test]
async fn owner_erase_removes_fact_and_abstraction_sidecar_matrix()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;
    let owner = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
    let fact_id = seed_fact(&pg, owner).await?;
    let abstraction_id = seed_agent_derivation(&pg, owner, fact_id).await?;

    let engine = compliance_engine(&pg);
    let outcome = engine
        .erase_abandoned_group_owner(
            &admin_authz_for(owner),
            match owner {
                OwnerRef::Group(group) => group,
                _ => unreachable!("test owner is group"),
            },
        )
        .await?;

    let ComplianceEraseOutcome::Completed { counts, .. } = outcome else {
        panic!("expected completed erase, got {outcome:?}");
    };
    assert_eq!(counts.memories, 2);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*)::bigint FROM proxima_core.memories WHERE memory_id = $1",
        )
        .bind(fact_id)
        .fetch_one(pg.pool_for_tests())
        .await?,
        0_i64
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*)::bigint FROM proxima_core.agent_derivation_v1 WHERE memory_id = $1",
        )
        .bind(abstraction_id)
        .fetch_one(pg.pool_for_tests())
        .await?,
        0_i64
    );

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}
