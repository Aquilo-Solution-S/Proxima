//! PR9 compliance erasure integration contracts.

use std::sync::Arc;

use crate::common::{
    engine_with_registry, owner_write_permit, storage_ports_with_compliance_and_drop_proof,
};
use proxima_core::access::{AccessError, Role};
use proxima_core::engine::Engine;
use proxima_core::storage_ports::{
    CitedObjectErasePort, ComplianceAdminPort, FactIngestPort, OwnerDropProofPort,
    OwnerMembershipAdminPort, OwnerWritePermit,
};
use proxima_core::verbs::fact_ingest::{FactReceiptDraft, FactWriteCommand};
use proxima_core::verbs::schema::{FlavorRegistryFrozen, PayloadKind, SchemaInfo};
use proxima_core::{
    AccessKind, AuthPath, AuthzContext, ComplianceEraseOutcome, ComplianceEraseRefusal,
    ComplianceEraseTarget, FactIngestOutcome, GroupId, OwnerRef, Relation, SchemaId, SchemaVersion,
    SourceBatchId, SourceId, StorageError, UserId,
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

#[derive(Debug)]
struct AllowDropProof;

#[async_trait::async_trait]
impl OwnerDropProofPort for AllowDropProof {
    async fn verify_personal_owner_dropped(
        &self,
        _user_id: UserId,
        _drop_event_id: &str,
    ) -> Result<bool, AccessError> {
        Ok(true)
    }
}

fn schemas_for_test() -> Vec<SchemaInfo> {
    vec![SchemaInfo {
        schema_id: SchemaId::new("test/compliance_fact".into()),
        schema_version: SchemaVersion::new(1),
        kind: PayloadKind::Fact,
        filter_keys: vec![],
        sidecar_table: None,
        natural_key_columns: vec![],
        tombstone: None,
        has_typed_ingress: false,
        cited_object_schema: None,
        embeddable: true,
    }]
}

fn compliance_engine(pg: &PgStorage) -> Engine {
    Engine::new(FlavorRegistryFrozen::with_schemas(schemas_for_test())).with_storage_ports(
        storage_ports_with_compliance_and_drop_proof(
            pg,
            Arc::new(AllowComplianceAdmin),
            Some(Arc::new(AllowDropProof)),
        ),
    )
}

/// `CitedObjectErasePort` that always fails, so an owner-scope erase leaves
/// the durable `cited_object_purge_pending` audit flag set — used to observe
/// the persisted state independently of the engine's own post-purge clear.
#[derive(Debug)]
struct FailingObjectPurge;

#[async_trait::async_trait]
impl CitedObjectErasePort for FailingObjectPurge {
    async fn purge_owner_objects(&self, _owner: OwnerRef) -> Result<u64, StorageError> {
        Err(StorageError::Unavailable(
            "object store unavailable in test".into(),
        ))
    }
}

fn compliance_engine_with_failing_purge(pg: &PgStorage) -> Engine {
    compliance_engine(pg).with_cited_object_erase(Arc::new(FailingObjectPurge))
}

fn engine_without_compliance_admin(pg: &PgStorage) -> Engine {
    engine_with_registry(pg, FlavorRegistryFrozen::with_schemas(schemas_for_test()))
}

fn receipt_draft(source_id: &str, batch: Uuid, payload: &[u8]) -> FactWriteCommand {
    let now = time::OffsetDateTime::now_utc();
    FactWriteCommand {
        schema_id: SchemaId::new("test/compliance_fact".into()),
        schema_version: SchemaVersion::new(1),
        payload: payload.to_vec(),
        rendered_text: Some(String::from_utf8_lossy(payload).to_string()),
        lexical_language: None,
        receipt: Some(FactReceiptDraft {
            source_id: SourceId::new(source_id),
            source_batch_id: SourceBatchId::new(batch),
            observed_at: now,
            occurred_at: now,
        }),
        citation: None,
        derived_from: None,
    }
}

async fn audit_count(pg: &PgStorage) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.compliance_audit_log")
        .fetch_one(pg.pool_for_tests())
        .await
}

fn admin_authz_for(owner: OwnerRef) -> AuthzContext {
    match owner {
        OwnerRef::Personal(user_id) => AuthzContext::for_subject(user_id, AuthPath::HostBearer),
        OwnerRef::Group(_) => AuthzContext::for_subject_with_role(
            UserId::new(Uuid::now_v7()),
            [(owner, Role::admin())],
            AuthPath::HostBearer,
        ),
        OwnerRef::World => AuthzContext::denied(),
    }
}

async fn fact_permit(owner: &OwnerRef) -> Result<OwnerWritePermit, StorageError> {
    owner_write_permit(owner, AccessKind::Fact)
        .await
        .map_err(|err| StorageError::Internal(err.to_string()))
}

async fn seed_fact(
    pg: &PgStorage,
    owner: &OwnerRef,
    draft: &FactWriteCommand,
) -> Result<FactIngestOutcome, StorageError> {
    let permit = fact_permit(owner).await?;
    pg.ingest_fact_atomic(&permit, draft, None).await
}

async fn seed_group_member(
    pg: &PgStorage,
    group: GroupId,
    member: UserId,
    relation: Relation,
) -> Result<(), StorageError> {
    let owner = OwnerRef::Group(group);
    let permit = owner_write_permit(&owner, AccessKind::Goal)
        .await
        .map_err(|err| StorageError::Internal(err.to_string()))?;
    pg.add_group_member(&permit, group, member, relation, Uuid::now_v7())
        .await
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OwnerContentCounts {
    memories: i64,
    goals: i64,
    edges: i64,
    fact_entities: i64,
    receipts: i64,
    source_batches: i64,
    citations: i64,
    cited_objects: i64,
}

async fn owner_content_counts(
    pg: &PgStorage,
    owner: OwnerRef,
) -> Result<OwnerContentCounts, sqlx::Error> {
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(&owner);
    let (
        memories,
        goals,
        edges,
        fact_entities,
        receipts,
        source_batches,
        citations,
        cited_objects,
    ): (i64, i64, i64, i64, i64, i64, i64, i64) = sqlx::query_as(
        "SELECT
            (SELECT count(*)::bigint FROM proxima_core.memories WHERE owner_kind = $1 AND owner_id IS NOT DISTINCT FROM $2),
            (SELECT count(*)::bigint FROM proxima_core.goals WHERE owner_kind = $1 AND owner_id IS NOT DISTINCT FROM $2),
            (SELECT count(*)::bigint FROM proxima_core.edges WHERE owner_kind = $1 AND owner_id IS NOT DISTINCT FROM $2),
            (SELECT count(*)::bigint FROM proxima_core.fact_entities WHERE owner_kind = $1 AND owner_id IS NOT DISTINCT FROM $2),
            (SELECT count(*)::bigint FROM proxima_core.fact_receipts WHERE owner_kind = $1 AND owner_id IS NOT DISTINCT FROM $2),
            (SELECT count(*)::bigint FROM proxima_core.source_batches WHERE owner_kind = $1 AND owner_id IS NOT DISTINCT FROM $2),
            (SELECT count(*)::bigint FROM proxima_core.citation_mappings WHERE owner_kind = $1 AND owner_id IS NOT DISTINCT FROM $2),
            (SELECT count(*)::bigint FROM proxima_core.cited_objects WHERE owner_kind = $1 AND owner_id IS NOT DISTINCT FROM $2)",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .fetch_one(pg.pool_for_tests())
    .await?;
    Ok(OwnerContentCounts {
        memories,
        goals,
        edges,
        fact_entities,
        receipts,
        source_batches,
        citations,
        cited_objects,
    })
}

fn assert_legal_hold_refusal(outcome: &ComplianceEraseOutcome) {
    assert!(matches!(
        outcome,
        ComplianceEraseOutcome::Refused {
            reason: ComplianceEraseRefusal::LegalHoldActive,
            ..
        }
    ));
}

struct SharedFactEntityFixture {
    erased_memory: Uuid,
    kept_memory: Uuid,
    fact_entity: Uuid,
    edge: Uuid,
}

async fn seed_shared_fact_entity_fixture(
    pg: &PgStorage,
    owner: OwnerRef,
) -> Result<SharedFactEntityFixture, Box<dyn std::error::Error>> {
    let erased_draft = receipt_draft("test/shared-entity-a", Uuid::now_v7(), b"shared-v1");
    let kept_draft = receipt_draft("test/shared-entity-b", Uuid::now_v7(), b"shared-v2");
    let erased = seed_fact(pg, &owner, &erased_draft).await?;
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    let kept = seed_fact(pg, &owner, &kept_draft).await?;
    let fact_entity_id = Uuid::now_v7();
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(&owner);
    sqlx::query(
        "INSERT INTO proxima_core.fact_entities(
            fact_entity_id, owner_kind, owner_id, schema_id, schema_version,
            natural_key, current_memory_id, current_created_at)
         SELECT $1, $2, $3, 'test/compliance-stateful', 1,
                ARRAY['shared-key']::text[], m.memory_id, m.created_at
           FROM proxima_core.memories m
          WHERE m.memory_id = $4",
    )
    .bind(fact_entity_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(erased.memory_id.into_inner())
    .execute(pg.pool_for_tests())
    .await?;
    sqlx::query(
        "UPDATE proxima_core.memories
            SET fact_entity_id = $1
          WHERE memory_id IN ($2, $3)",
    )
    .bind(fact_entity_id)
    .bind(erased.memory_id.into_inner())
    .bind(kept.memory_id.into_inner())
    .execute(pg.pool_for_tests())
    .await?;
    let edge_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxima_core.edges(
            edge_id, owner_kind, owner_id, relation, relation_class,
            source_kind, source_memory_id, source_goal_id, source_fact_entity_id,
            target_kind, target_memory_id, target_goal_id, target_fact_entity_id,
            authorship_kind, authorship_owner_memory_id)
         VALUES ($1, $2, $3, 'test/compliance/shared-entity-source',
                 'Structural'::proxima_core.relation_class,
                 'Fact'::proxima_core.entity_kind, NULL, NULL, $4,
                 'Fact'::proxima_core.entity_kind, $5, NULL, NULL,
                 'Engine'::proxima_core.edge_authorship_kind, NULL)",
    )
    .bind(edge_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(fact_entity_id)
    .bind(kept.memory_id.into_inner())
    .execute(pg.pool_for_tests())
    .await?;
    Ok(SharedFactEntityFixture {
        erased_memory: erased.memory_id.into_inner(),
        kept_memory: kept.memory_id.into_inner(),
        fact_entity: fact_entity_id,
        edge: edge_id,
    })
}

async fn assert_shared_fact_entity_survives_source_scope_erase(
    pg: &PgStorage,
    fixture: &SharedFactEntityFixture,
) -> Result<(), sqlx::Error> {
    let erased_remaining: i64 = sqlx::query_scalar(
        "SELECT count(*)::bigint FROM proxima_core.memories WHERE memory_id = $1",
    )
    .bind(fixture.erased_memory)
    .fetch_one(pg.pool_for_tests())
    .await?;
    assert_eq!(erased_remaining, 0);
    let kept_entity: Option<Uuid> =
        sqlx::query_scalar("SELECT fact_entity_id FROM proxima_core.memories WHERE memory_id = $1")
            .bind(fixture.kept_memory)
            .fetch_one(pg.pool_for_tests())
            .await?;
    assert_eq!(kept_entity, Some(fixture.fact_entity));
    let current_memory: Option<Uuid> = sqlx::query_scalar(
        "SELECT current_memory_id FROM proxima_core.fact_entities WHERE fact_entity_id = $1",
    )
    .bind(fixture.fact_entity)
    .fetch_optional(pg.pool_for_tests())
    .await?;
    assert_eq!(current_memory, Some(fixture.kept_memory));
    let edge_rows: i64 = sqlx::query_scalar(
        "SELECT count(*)::bigint
           FROM proxima_core.edges
          WHERE edge_id = $1 AND source_fact_entity_id = $2",
    )
    .bind(fixture.edge)
    .bind(fixture.fact_entity)
    .fetch_one(pg.pool_for_tests())
    .await?;
    assert_eq!(edge_rows, 1, "source fact-entity edge must survive");
    Ok(())
}

mod legal_hold;
mod owner_scope;
mod refusals;
mod source_scope;
