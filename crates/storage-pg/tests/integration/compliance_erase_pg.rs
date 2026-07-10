//! PR9 compliance erasure integration contracts.

use std::sync::Arc;

use crate::common::{
    create_db, db_url, drop_db, engine_with_registry, owner_write_permit, seed_memory,
    seed_memory_edge, storage_ports_with_compliance_and_drop_proof,
};
use proxima_core::access::{AccessError, Role};
use proxima_core::change_event::EdgeTargetProjection;
use proxima_core::engine::Engine;
use proxima_core::storage_ports::{
    ComplianceAdminPort, EdgeReadPort, FactIngestPort, OwnerDropProofPort,
    OwnerMembershipAdminPort, OwnerWritePermit,
};
use proxima_core::verbs::fact_ingest::{FactReceiptDraft, FactWriteCommand};
use proxima_core::verbs::query::{EdgeFilter, EdgeReadRequest, QueryRequest};
use proxima_core::verbs::schema::{FlavorRegistryFrozen, PayloadKind, SchemaInfo};
use proxima_core::{
    AccessKind, AuthPath, AuthzContext, ComplianceEraseCounts, ComplianceEraseOutcome,
    ComplianceEraseRefusal, ComplianceEraseTarget, EdgeId, EntityKind, FactIngestOutcome, GroupId,
    OwnerRef, Relation, RelationClass, SchemaId, SchemaVersion, SourceBatchId, SourceId,
    StorageError, UserId,
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

fn compliance_engine(pg: &PgStorage) -> Engine {
    Engine::new(FlavorRegistryFrozen::with_schemas(schemas_for_test())).with_storage_ports(
        storage_ports_with_compliance_and_drop_proof(
            pg,
            Arc::new(AllowComplianceAdmin),
            Some(Arc::new(AllowDropProof)),
        ),
    )
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
        source_cursors: 15,
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
        cited_object_purge_pending: false,
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
        seed_group_member(&pg, group, member, Relation::Admin).await?;
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
        let first = seed_fact(&pg, &owner, &draft).await?;

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
            .ingest_fact_atomic(&fact_permit(&owner).await?, &draft, None)
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

        let held_owner = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
        engine
            .set_legal_hold(&admin_authz_for(held_owner), &held_owner)
            .await?;
        let held_outcome = engine
            .erase_world_owner(&AuthzContext::for_subject(
                UserId::new(Uuid::now_v7()),
                AuthPath::HostBearer,
            ))
            .await?;
        assert!(matches!(
            held_outcome,
            ComplianceEraseOutcome::Refused {
                reason: ComplianceEraseRefusal::WorldOwner,
                ..
            }
        ));
        assert_eq!(audit_count(&pg).await?, 2);
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn legal_hold_round_trips_and_set_is_idempotent() -> Result<(), Box<dyn std::error::Error>> {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await?;
    let url = db_url(&db_name);
    let result = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let engine = compliance_engine(&pg);
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let authz = admin_authz_for(owner);

        assert!(!engine.get_legal_hold(&authz, &owner).await?);
        engine.set_legal_hold(&authz, &owner).await?;
        assert!(engine.get_legal_hold(&authz, &owner).await?);
        engine.set_legal_hold(&authz, &owner).await?;
        assert!(engine.get_legal_hold(&authz, &owner).await?);
        assert!(engine.clear_legal_hold(&authz, &owner).await?);
        assert!(!engine.get_legal_hold(&authz, &owner).await?);
        assert!(!engine.clear_legal_hold(&authz, &owner).await?);
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn owner_admin_can_read_but_not_set_or_clear_legal_hold()
-> Result<(), Box<dyn std::error::Error>> {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await?;
    let url = db_url(&db_name);
    let result = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let engine = engine_without_compliance_admin(&pg);
        let operator_engine = compliance_engine(&pg);
        let group = GroupId::new(Uuid::now_v7());
        let owner = OwnerRef::Group(group);
        let owner_admin = AuthzContext::for_subject_with_role(
            UserId::new(Uuid::now_v7()),
            [(owner, Role::admin())],
            AuthPath::HostBearer,
        );
        let operator = admin_authz_for(owner);

        assert!(!engine.get_legal_hold(&owner_admin, &owner).await?);
        let set_err = engine
            .set_legal_hold(&owner_admin, &owner)
            .await
            .expect_err("owner admin without operator authority cannot set hold");
        assert_eq!(set_err.code, proxima_core::ErrorCode::Forbidden);
        assert!(
            !engine.get_legal_hold(&owner_admin, &owner).await?,
            "denied set must leave hold inactive"
        );

        operator_engine.set_legal_hold(&operator, &owner).await?;
        assert!(engine.get_legal_hold(&owner_admin, &owner).await?);
        let clear_err = engine
            .clear_legal_hold(&owner_admin, &owner)
            .await
            .expect_err("owner admin without operator authority cannot clear hold");
        assert_eq!(clear_err.code, proxima_core::ErrorCode::Forbidden);
        assert!(
            engine.get_legal_hold(&owner_admin, &owner).await?,
            "denied clear must leave hold active"
        );

        assert!(operator_engine.clear_legal_hold(&operator, &owner).await?);
        assert!(!engine.get_legal_hold(&owner_admin, &owner).await?);
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn legal_hold_blocks_destructive_erase_verbs_without_deleting_rows()
-> Result<(), Box<dyn std::error::Error>> {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await?;
    let url = db_url(&db_name);
    let result = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let engine = compliance_engine(&pg);

        let group = GroupId::new(Uuid::now_v7());
        let group_owner = OwnerRef::Group(group);
        seed_fact(
            &pg,
            &group_owner,
            &receipt_draft("hold/group-a", Uuid::now_v7(), b"held-group-a"),
        )
        .await?;
        seed_fact(
            &pg,
            &group_owner,
            &receipt_draft("hold/group-b", Uuid::now_v7(), b"held-group-b"),
        )
        .await?;
        let group_authz = admin_authz_for(group_owner);
        engine.set_legal_hold(&group_authz, &group_owner).await?;
        let group_counts = owner_content_counts(&pg, group_owner).await?;

        let outcome = engine
            .erase_abandoned_group_owner(
                &AuthzContext::for_subject(UserId::new(Uuid::now_v7()), AuthPath::HostBearer),
                group,
            )
            .await?;
        assert_legal_hold_refusal(&outcome);
        assert_eq!(owner_content_counts(&pg, group_owner).await?, group_counts);

        let outcome = engine
            .erase_abandoned_group_source_scope(
                &AuthzContext::for_subject(UserId::new(Uuid::now_v7()), AuthPath::HostBearer),
                group,
                SourceId::new("hold/group-a"),
            )
            .await?;
        assert_legal_hold_refusal(&outcome);
        assert_eq!(owner_content_counts(&pg, group_owner).await?, group_counts);

        let user = UserId::new(Uuid::now_v7());
        let personal_owner = OwnerRef::Personal(user);
        seed_fact(
            &pg,
            &personal_owner,
            &receipt_draft("hold/personal-a", Uuid::now_v7(), b"held-personal-a"),
        )
        .await?;
        seed_fact(
            &pg,
            &personal_owner,
            &receipt_draft("hold/personal-b", Uuid::now_v7(), b"held-personal-b"),
        )
        .await?;
        let personal_authz = admin_authz_for(personal_owner);
        engine
            .set_legal_hold(&personal_authz, &personal_owner)
            .await?;
        let personal_counts = owner_content_counts(&pg, personal_owner).await?;

        let outcome = engine
            .erase_dropped_personal_owner(
                &AuthzContext::for_subject(UserId::new(Uuid::now_v7()), AuthPath::HostBearer),
                user,
                "drop-ok".to_owned(),
            )
            .await?;
        assert_legal_hold_refusal(&outcome);
        assert_eq!(
            owner_content_counts(&pg, personal_owner).await?,
            personal_counts
        );

        let outcome = engine
            .erase_dropped_personal_source_scope(
                &AuthzContext::for_subject(UserId::new(Uuid::now_v7()), AuthPath::HostBearer),
                user,
                SourceId::new("hold/personal-a"),
                "drop-ok".to_owned(),
            )
            .await?;
        assert_legal_hold_refusal(&outcome);
        assert_eq!(
            owner_content_counts(&pg, personal_owner).await?,
            personal_counts
        );
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn legal_hold_does_not_block_reads_or_ordinary_writes()
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
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
        engine.set_legal_hold(&authz, &owner).await?;

        let written = engine
            .fact_ingest(
                &authz,
                receipt_draft("hold/write", Uuid::now_v7(), b"write-while-held"),
            )
            .await?;
        let read = engine
            .query(&authz, &QueryRequest::for_owner(owner))
            .await?;
        assert!(read.memories.iter().any(|row| row.id == written.memory_id));
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn clearing_legal_hold_restores_prior_erase_behavior()
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
        let written = seed_fact(
            &pg,
            &owner,
            &receipt_draft("hold/clear", Uuid::now_v7(), b"clear-then-erase"),
        )
        .await?;
        let authz = admin_authz_for(owner);
        engine.set_legal_hold(&authz, &owner).await?;

        let refused = engine
            .erase_abandoned_group_owner(
                &AuthzContext::for_subject(UserId::new(Uuid::now_v7()), AuthPath::HostBearer),
                group,
            )
            .await?;
        assert_legal_hold_refusal(&refused);

        assert!(engine.clear_legal_hold(&authz, &owner).await?);
        let completed = engine
            .erase_abandoned_group_owner(
                &AuthzContext::for_subject(UserId::new(Uuid::now_v7()), AuthPath::HostBearer),
                group,
            )
            .await?;
        assert!(matches!(
            completed,
            ComplianceEraseOutcome::Completed { .. }
        ));
        let remaining: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.memories WHERE memory_id = $1",
        )
        .bind(written.memory_id.into_inner())
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(remaining, 0);
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn legal_hold_on_one_owner_does_not_affect_another_owner_erase()
-> Result<(), Box<dyn std::error::Error>> {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await?;
    let url = db_url(&db_name);
    let result = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let engine = compliance_engine(&pg);
        let held_group = GroupId::new(Uuid::now_v7());
        let held_owner = OwnerRef::Group(held_group);
        seed_fact(
            &pg,
            &held_owner,
            &receipt_draft("hold/owner-a", Uuid::now_v7(), b"held-owner-a"),
        )
        .await?;
        let held_counts = owner_content_counts(&pg, held_owner).await?;
        engine
            .set_legal_hold(&admin_authz_for(held_owner), &held_owner)
            .await?;

        let free_group = GroupId::new(Uuid::now_v7());
        let free_owner = OwnerRef::Group(free_group);
        let free = seed_fact(
            &pg,
            &free_owner,
            &receipt_draft("hold/owner-b", Uuid::now_v7(), b"free-owner-b"),
        )
        .await?;
        let outcome = engine
            .erase_abandoned_group_owner(
                &AuthzContext::for_subject(UserId::new(Uuid::now_v7()), AuthPath::HostBearer),
                free_group,
            )
            .await?;
        assert!(matches!(outcome, ComplianceEraseOutcome::Completed { .. }));
        let free_remaining: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.memories WHERE memory_id = $1",
        )
        .bind(free.memory_id.into_inner())
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(free_remaining, 0);
        assert_eq!(owner_content_counts(&pg, held_owner).await?, held_counts);
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
        let erased = seed_fact(&pg, &owner, &erased_draft).await?;
        let kept = seed_fact(&pg, &owner, &kept_draft).await?;
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
            .ingest_fact_atomic(&fact_permit(&owner).await?, &replay, None)
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
        let erased = seed_fact(&pg, &owner, &erased_draft).await?;
        let kept = seed_fact(&pg, &owner, &kept_draft).await?;

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

/// An owner erase physically
/// removes the owner's persisted projector cursors and counts them, so a
/// re-provisioned owner never resumes from a stale offset.
#[tokio::test]
async fn abandoned_group_owner_erase_removes_source_cursors()
-> Result<(), Box<dyn std::error::Error>> {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await?;
    let url = db_url(&db_name);
    let result = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let engine = compliance_engine(&pg);
        let group_uuid = Uuid::now_v7();
        let group = GroupId::new(group_uuid);
        let owner = OwnerRef::Group(group);
        let authz = AuthzContext::for_subject(UserId::new(Uuid::now_v7()), AuthPath::HostBearer);
        let draft = receipt_draft("test/source", Uuid::now_v7(), b"erase-me");
        seed_fact(&pg, &owner, &draft).await?;

        sqlx::query(
            "INSERT INTO proxima_core.source_cursors (owner_kind, owner_id, source, cursor)
             VALUES ('group', $1, 'test/source', $2)",
        )
        .bind(group_uuid)
        .bind(&b"opaque-offset"[..])
        .execute(pg.pool_for_tests())
        .await?;

        let outcome = engine.erase_abandoned_group_owner(&authz, group).await?;
        let ComplianceEraseOutcome::Completed { counts, .. } = outcome else {
            panic!("expected completed erase, got {outcome:?}");
        };
        assert_eq!(
            counts.source_cursors, 1,
            "owner erase counts the deleted cursor"
        );

        let remaining: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.source_cursors WHERE owner_id = $1",
        )
        .bind(group_uuid)
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(
            remaining, 0,
            "the cursor is physically erased with the owner"
        );
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}
