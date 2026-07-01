//! PR9 compliance erasure integration contracts.

use std::sync::Arc;

use crate::common::{create_db, db_url, drop_db, seed_memory, seed_memory_edge};
use proxima_core::access::AccessError;
use proxima_core::change_event::EdgeTargetProjection;
use proxima_core::engine::Engine;
use proxima_core::storage_ports::{
    ComplianceAdminPort, EdgeReadPort, FactIngestPort, OwnerDropProofPort,
    OwnerMembershipAdminPort, StoragePorts,
};
use proxima_core::verbs::fact_ingest::{FactReceiptDraft, FactWriteCommand};
use proxima_core::verbs::query::{EdgeFilter, EdgeReadRequest};
use proxima_core::verbs::schema::{FlavorRegistryFrozen, PayloadKind, SchemaInfo};
use proxima_core::{
    AuthPath, AuthzContext, ComplianceEraseCounts, ComplianceEraseOutcome, ComplianceEraseRefusal,
    ComplianceEraseTarget, EdgeId, EntityKind, GroupId, OwnerRef, Relation, RelationClass,
    SchemaId, SchemaVersion, SourceBatchId, SourceId, UserId,
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
    }]
}

fn storage_ports_with_compliance(pg: &PgStorage) -> StoragePorts {
    let pg = Arc::new(pg.clone());
    StoragePorts::builder()
        .fact_ingest(pg.clone())
        .mcp_call_write(pg.clone())
        .mcp_call_read(pg.clone())
        .memory_authoring(pg.clone())
        .memory_read(pg.clone())
        .memory_inspect(pg.clone())
        .embedding_text(pg.clone())
        .embedding_write(pg.clone())
        .embedding_job(pg.clone())
        .goal_write(pg.clone())
        .goal_read(pg.clone())
        .change_event(pg.clone())
        .edge_read(pg.clone())
        .citation(pg.clone())
        .owner_access_read(pg.clone())
        .owner_membership_admin(pg.clone())
        .source_batch(pg.clone())
        .fact_retention(pg.clone())
        .compliance_erase(pg.clone())
        .compliance_admin(Arc::new(AllowComplianceAdmin))
        .owner_drop_proof(Arc::new(AllowDropProof))
        .registry_projection(pg)
        .build()
}

fn compliance_engine(pg: &PgStorage) -> Engine {
    Engine::new(FlavorRegistryFrozen::with_schemas(schemas_for_test()))
        .with_storage_ports(storage_ports_with_compliance(pg))
}

fn receipt_draft(source_id: &str, batch: Uuid, payload: &[u8]) -> FactWriteCommand {
    let now = time::OffsetDateTime::now_utc();
    FactWriteCommand {
        schema_id: SchemaId::new("test/compliance_fact".into()),
        schema_version: SchemaVersion::new(1),
        payload: payload.to_vec(),
        rendered_text: Some(String::from_utf8_lossy(payload).to_string()),
        receipt: Some(FactReceiptDraft {
            source_id: SourceId::new(source_id),
            source_batch_id: SourceBatchId::new(batch),
            observed_at: now,
            occurred_at: now,
        }),
        citation: None,
    }
}

async fn audit_count(pg: &PgStorage) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.compliance_audit_log")
        .fetch_one(pg.pool_for_tests())
        .await
}

#[test]
fn compliance_outcome_counts_are_content_free() {
    let counts = ComplianceEraseCounts {
        memories: 1,
        goals: 2,
        edges: 3,
        fact_entities: 4,
        receipts: 5,
        source_batches: 6,
        citations: 7,
        cited_objects: 8,
        embeddings: 9,
        embedding_jobs: 10,
        mcp_call_rows: 11,
        change_events: 12,
        redacted_edge_targets: 13,
        suppressed_keys: 14,
    };
    let outcome = ComplianceEraseOutcome::Completed {
        operation_id: Uuid::now_v7(),
        counts,
    };
    assert!(matches!(
        outcome,
        ComplianceEraseOutcome::Completed {
            counts: ComplianceEraseCounts { memories: 1, .. },
            ..
        }
    ));
}

#[test]
fn world_owner_refusal_has_a_typed_reason() {
    let outcome = ComplianceEraseOutcome::Refused {
        operation_id: Uuid::now_v7(),
        reason: ComplianceEraseRefusal::WorldOwner,
    };
    assert!(matches!(
        outcome,
        ComplianceEraseOutcome::Refused {
            reason: ComplianceEraseRefusal::WorldOwner,
            ..
        }
    ));
}

#[tokio::test]
async fn group_owner_with_member_is_refused_and_audited() -> Result<(), Box<dyn std::error::Error>>
{
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await?;
    let url = db_url(&db_name);
    let result = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let engine = compliance_engine(&pg);
        let group = GroupId::new(Uuid::now_v7());
        let member = UserId::new(Uuid::now_v7());
        pg.add_group_member(group, member, Relation::Admin, Uuid::now_v7())
            .await?;
        let authz = AuthzContext::for_subject(member, AuthPath::HostBearer);

        let outcome = engine.erase_abandoned_group_owner(&authz, group).await?;
        assert!(matches!(
            outcome,
            ComplianceEraseOutcome::Refused {
                reason: ComplianceEraseRefusal::OwnerNotAbandoned,
                ..
            }
        ));
        assert_eq!(audit_count(&pg).await?, 1);
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn abandoned_group_owner_erases_owned_fact_and_suppresses_reingest()
-> Result<(), Box<dyn std::error::Error>> {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await?;
    let url = db_url(&db_name);
    let result = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let engine = compliance_engine(&pg);
        let group = GroupId::new(Uuid::now_v7());
        let owner = OwnerRef::Group(group);
        let authz = AuthzContext::for_subject(UserId::new(Uuid::now_v7()), AuthPath::HostBearer);
        let draft = receipt_draft("test/source", Uuid::now_v7(), b"erase-me");
        let first = pg.ingest_fact_atomic(&owner, &draft, None).await?;

        let outcome = engine.erase_abandoned_group_owner(&authz, group).await?;
        let ComplianceEraseOutcome::Completed { counts, .. } = outcome else {
            panic!("expected completed erase, got {outcome:?}");
        };
        assert_eq!(counts.memories, 1);
        assert!(counts.receipts >= 1);
        assert!(counts.suppressed_keys >= 1);

        let remaining: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.memories WHERE memory_id = $1",
        )
        .bind(first.memory_id.into_inner())
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(remaining, 0);

        let suppressed = pg
            .ingest_fact_atomic(&owner, &draft, None)
            .await
            .expect_err("suppression must block reingest before receipt replay");
        assert!(matches!(
            suppressed,
            proxima_core::StorageError::Suppressed(_)
        ));
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn target_abandoned_keeps_live_source_edge_as_unavailable()
-> Result<(), Box<dyn std::error::Error>> {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await?;
    let url = db_url(&db_name);
    let result = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let engine = compliance_engine(&pg);
        let live = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let group = GroupId::new(Uuid::now_v7());
        let target_owner = OwnerRef::Group(group);
        let source = seed_memory(&pg, &live, EntityKind::Fact, "source").await?;
        let target = seed_memory(&pg, &target_owner, EntityKind::Fact, "target").await?;
        let edge = seed_memory_edge(
            &pg,
            &live,
            (EntityKind::Fact, source),
            (EntityKind::Fact, target),
            "test/compliance/mentions",
            RelationClass::Structural,
        )
        .await?;

        let outcome = engine
            .erase_abandoned_group_owner(
                &AuthzContext::for_subject(UserId::new(Uuid::now_v7()), AuthPath::HostBearer),
                group,
            )
            .await?;
        assert!(matches!(outcome, ComplianceEraseOutcome::Completed { .. }));

        let edge_rows: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.edges WHERE edge_id = $1",
        )
        .bind(edge.into_inner())
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(edge_rows, 1, "live source-owned edge row survives");

        let redactions: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.compliance_edge_target_redactions WHERE edge_id = $1",
        )
        .bind(edge.into_inner())
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(redactions, 1);

        let read = pg
            .read_edges(
                &[live],
                &EdgeReadRequest {
                    owner: live,
                    edge_ids: vec![EdgeId::new(edge.into_inner())],
                    filter: EdgeFilter::default(),
                    limit: 10,
                },
            )
            .await?;
        assert_eq!(read.edges.len(), 1);
        assert_eq!(read.edges[0].target, EdgeTargetProjection::Unavailable);
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn world_owner_erase_refuses_and_audits() -> Result<(), Box<dyn std::error::Error>> {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await?;
    let url = db_url(&db_name);
    let result = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let engine = compliance_engine(&pg);

        let outcome = engine
            .erase_world_owner(&AuthzContext::for_subject(
                UserId::new(Uuid::now_v7()),
                AuthPath::HostBearer,
            ))
            .await?;
        assert!(matches!(
            outcome,
            ComplianceEraseOutcome::Refused {
                reason: ComplianceEraseRefusal::WorldOwner,
                ..
            }
        ));
        assert_eq!(audit_count(&pg).await?, 1);
        let target_kind: String =
            sqlx::query_scalar("SELECT target_kind FROM proxima_core.compliance_audit_log LIMIT 1")
                .fetch_one(pg.pool_for_tests())
                .await?;
        assert_eq!(target_kind, "WorldOwner");
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn group_source_scope_erases_only_requested_source_and_suppresses_new_batches()
-> Result<(), Box<dyn std::error::Error>> {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await?;
    let url = db_url(&db_name);
    let result = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let engine = compliance_engine(&pg);
        let group = GroupId::new(Uuid::now_v7());
        let owner = OwnerRef::Group(group);
        let erased_draft = receipt_draft("test/source-a", Uuid::now_v7(), b"erase-source-a");
        let kept_draft = receipt_draft("test/source-b", Uuid::now_v7(), b"keep-source-b");
        let erased = pg.ingest_fact_atomic(&owner, &erased_draft, None).await?;
        let kept = pg.ingest_fact_atomic(&owner, &kept_draft, None).await?;
        let surviving_edge = seed_memory_edge(
            &pg,
            &owner,
            (EntityKind::Fact, kept.memory_id),
            (EntityKind::Fact, erased.memory_id),
            "test/compliance/source-scope-mentions",
            RelationClass::Structural,
        )
        .await?;

        let outcome = engine
            .erase_abandoned_group_source_scope(
                &AuthzContext::for_subject(UserId::new(Uuid::now_v7()), AuthPath::HostBearer),
                group,
                SourceId::new("test/source-a"),
            )
            .await?;
        assert!(matches!(outcome, ComplianceEraseOutcome::Completed { .. }));

        let erased_remaining: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.memories WHERE memory_id = $1",
        )
        .bind(erased.memory_id.into_inner())
        .fetch_one(pg.pool_for_tests())
        .await?;
        let kept_remaining: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.memories WHERE memory_id = $1",
        )
        .bind(kept.memory_id.into_inner())
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(erased_remaining, 0);
        assert_eq!(kept_remaining, 1);
        let edge_rows = pg
            .read_edges(
                &[owner],
                &EdgeReadRequest {
                    owner,
                    edge_ids: vec![EdgeId::new(surviving_edge.into_inner())],
                    filter: EdgeFilter::default(),
                    limit: 10,
                },
            )
            .await?;
        assert_eq!(edge_rows.edges.len(), 1);
        assert_eq!(edge_rows.edges[0].target, EdgeTargetProjection::Unavailable);

        let replay = receipt_draft("test/source-a", Uuid::now_v7(), b"new-source-a");
        let suppressed = pg
            .ingest_fact_atomic(&owner, &replay, None)
            .await
            .expect_err("owner/source suppression blocks new batches before dedup");
        assert!(matches!(
            suppressed,
            proxima_core::StorageError::Suppressed(_)
        ));
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
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
    let erased = pg.ingest_fact_atomic(&owner, &erased_draft, None).await?;
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    let kept = pg.ingest_fact_atomic(&owner, &kept_draft, None).await?;
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

#[tokio::test]
async fn source_scope_erase_preserves_shared_fact_entity_head_and_source_edges()
-> Result<(), Box<dyn std::error::Error>> {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await?;
    let url = db_url(&db_name);
    let result = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let engine = compliance_engine(&pg);
        let group = GroupId::new(Uuid::now_v7());
        let owner = OwnerRef::Group(group);
        let fixture = seed_shared_fact_entity_fixture(&pg, owner).await?;

        let outcome = engine
            .erase_abandoned_group_source_scope(
                &AuthzContext::for_subject(UserId::new(Uuid::now_v7()), AuthPath::HostBearer),
                group,
                SourceId::new("test/shared-entity-a"),
            )
            .await?;
        let ComplianceEraseOutcome::Completed { counts, .. } = outcome else {
            panic!("expected completed source-scope erase");
        };
        assert_eq!(counts.fact_entities, 0, "shared fact entity must survive");

        assert_shared_fact_entity_survives_source_scope_erase(&pg, &fixture).await?;
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
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

#[tokio::test]
async fn personal_source_scope_with_verified_drop_erases_only_scope()
-> Result<(), Box<dyn std::error::Error>> {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await?;
    let url = db_url(&db_name);
    let result = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let engine = compliance_engine(&pg);
        let user = UserId::new(Uuid::now_v7());
        let owner = OwnerRef::Personal(user);
        let erased_draft = receipt_draft("personal/source-a", Uuid::now_v7(), b"erase-personal-a");
        let kept_draft = receipt_draft("personal/source-b", Uuid::now_v7(), b"keep-personal-b");
        let erased = pg.ingest_fact_atomic(&owner, &erased_draft, None).await?;
        let kept = pg.ingest_fact_atomic(&owner, &kept_draft, None).await?;

        let outcome = engine
            .erase_dropped_personal_source_scope(
                &AuthzContext::for_subject(UserId::new(Uuid::now_v7()), AuthPath::HostBearer),
                user,
                SourceId::new("personal/source-a"),
                "drop-ok".to_owned(),
            )
            .await?;
        assert!(matches!(outcome, ComplianceEraseOutcome::Completed { .. }));
        let counts: (i64, i64) = sqlx::query_as(
            "SELECT
                count(*) FILTER (WHERE memory_id = $1)::bigint,
                count(*) FILTER (WHERE memory_id = $2)::bigint
               FROM proxima_core.memories",
        )
        .bind(erased.memory_id.into_inner())
        .bind(kept.memory_id.into_inner())
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(counts, (0, 1));
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}
