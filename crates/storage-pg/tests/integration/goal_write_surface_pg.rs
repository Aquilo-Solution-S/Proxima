//! Public typed `GoalWrite` surface for embedded product hosts.

use proxima_core::storage_ports::*;
use std::sync::Arc;

use crate::common::{create_db, db_url, drop_db};

use proxima_core::authz::AuthPath;
use proxima_core::verbs::goal_write::{
    GoalAssignmentTarget, GoalCreateRequest, GoalEvidenceRef, GoalWakeConfigWrite, GoalWakeToolId,
    GoalWakeTrigger, IdempotencyKey,
};
use proxima_core::{
    AuthzContext, Engine, ErrorCode, FlavorRegistry, GoalPayload, GroupId, MemoryId, Owner,
    OwnerRef, PayloadKeyBuilder, Relation, Role, SchemaId, SchemaVersion, UserId,
};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

type ResolvedAuthz = AuthzContext;

const REQUEST_ID: &str = "product:onboarding:initial-goal:1";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct ProductInitialGoal {
    external_goal_id: String,
}

impl GoalPayload for ProductInitialGoal {
    const SCHEMA_ID: &'static str = "test/product-initial-goal-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn goal_key(&self) -> Vec<u8> {
        let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
        key.field_str("external_goal_id", &self.external_goal_id);
        key.finish()
    }
}

async fn insert_self(pg: &PgStorage, owner: &Owner) -> Result<MemoryId, sqlx::Error> {
    insert_memory(pg, owner, proxima_core::EntityKind::Perspective).await
}

async fn insert_evidence(pg: &PgStorage, owner: &Owner) -> Result<MemoryId, sqlx::Error> {
    insert_memory(pg, owner, proxima_core::EntityKind::Abstraction).await
}

async fn insert_memory(
    pg: &PgStorage,
    owner: &Owner,
    kind: proxima_core::EntityKind,
) -> Result<MemoryId, sqlx::Error> {
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(owner);
    let memory_id = Uuid::now_v7();
    let (schema_id, text, operator_kind) = match kind {
        proxima_core::EntityKind::Perspective => (
            "test/product-self",
            "product self",
            proxima_core::MemoryOperatorKind::AtoP,
        ),
        proxima_core::EntityKind::Abstraction => (
            "test/product-evidence",
            "product evidence",
            proxima_core::MemoryOperatorKind::AtoA,
        ),
        proxima_core::EntityKind::Fact | proxima_core::EntityKind::Goal => {
            unreachable!("test helper only inserts memory-backed A/P rows")
        }
    };
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_kind, owner_id, schema_id, schema_version, kind, text,
             operator_kind, operator_id, input_contract_id, source_batch_id, model_id, prompt_version)
         VALUES ($1, $2, $3, $4, 1, $5, $6, $7,
                 '00000000-0000-0000-0000-000000000341'::uuid,
                 '00000000-0000-0000-0000-000000000342'::uuid,
                 NULL, 'test-model', 'v1')"
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(schema_id)
    .bind(kind)
    .bind(text)
    .bind(operator_kind)
    .execute(pg.pool_for_tests())
    .await?;
    Ok(MemoryId::new(memory_id))
}

fn product_request(
    owner: &Owner,
    target_self: MemoryId,
    text: &str,
) -> GoalCreateRequest<ProductInitialGoal> {
    GoalCreateRequest::product(
        *owner,
        GoalAssignmentTarget::perspective(target_self),
        IdempotencyKey::new(REQUEST_ID).expect("stable request id is valid"),
        "Practice goal",
        text,
        ProductInitialGoal {
            external_goal_id: "weekday-practice".to_string(),
        },
    )
}

fn wake_config(trigger: GoalWakeTrigger, hard_memory_ids: &[MemoryId]) -> GoalWakeConfigWrite {
    let registry = FlavorRegistry::new().freeze_or_panic_for_tests();
    let search =
        GoalWakeToolId::parse("core_search_memories", &registry).expect("registered search tool");
    GoalWakeConfigWrite::new(trigger, vec![search], "wake prompt", hard_memory_ids)
        .expect("wake config shape")
}

async fn assert_no_goal_rows(pg: &PgStorage) -> Result<(), sqlx::Error> {
    let goal_count: (i64,) = sqlx::query_as("SELECT count(*)::bigint FROM proxima_core.goals")
        .fetch_one(pg.pool_for_tests())
        .await?;
    assert_eq!(goal_count.0, 0);
    Ok(())
}

fn assert_idempotency_conflict(err: &proxima_core::error::ProtocolError) {
    assert_eq!(err.code, ErrorCode::IdempotencyConflict);
}

async fn seed_group_membership(
    pg: &PgStorage,
    space_owner: &OwnerRef,
    relation: Relation,
    subject: &OwnerRef,
) {
    let OwnerRef::Group(group) = space_owner else {
        panic!("group membership can only seed group-owned spaces");
    };
    let OwnerRef::Personal(user) = subject else {
        panic!("group membership can only seed user members");
    };
    pg.add_group_member(*group, *user, relation, Uuid::now_v7())
        .await
        .expect("seed group membership");
}

async fn viewer_without_memory_write(pg: &PgStorage, owner: &Owner) -> ResolvedAuthz {
    let viewer = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
    seed_group_membership(pg, owner, Relation::Viewer, &viewer).await;
    let OwnerRef::Personal(user) = viewer else {
        panic!("viewer principal must be a user");
    };
    AuthzContext::for_subject_with_role(user, [(*owner, Role::viewer())], AuthPath::HostBearer)
}

async fn boot_registered(
    url: &str,
) -> Result<(PgStorage, Owner, MemoryId, Engine, AuthzContext), Box<dyn std::error::Error>> {
    let pg = PgStorage::connect(url).await?;
    pg.run_migrations().await?;
    let owner = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
    let target_self = insert_self(&pg, &owner).await?;
    let engine =
        Engine::compose_or_panic_for_tests(Arc::new(pg.clone()).storage_ports(), |registry| {
            registry.add_goal_schema_or_panic_for_tests::<ProductInitialGoal>();
        });
    let authz = AuthzContext::for_subject_with_role(
        UserId::new(Uuid::now_v7()),
        [(owner, Role::admin())],
        AuthPath::System,
    );
    Ok((pg, owner, target_self, engine, authz))
}

#[tokio::test]
async fn engine_goalwrite_writes_product_goal_idempotently_without_table_sql() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let (pg, owner, target_self, engine, authz) = boot_registered(&url).await?;
        let expected_key = ProductInitialGoal {
            external_goal_id: "weekday-practice".to_string(),
        }
        .goal_key();

        let outcome = engine
            .create_goal(
                &authz,
                product_request(&owner, target_self, "Practice every weekday."),
            )
            .await?;
        assert!(!outcome.idempotent_replay);
        assert!(outcome.lifecycle_memory_id.is_some());
        assert!(!outcome.edge_ids.is_empty());

        let replay = engine
            .create_goal(
                &authz,
                product_request(&owner, target_self, "Practice every weekday."),
            )
            .await?;
        assert!(replay.idempotent_replay);
        assert_eq!(replay.goal_id, outcome.goal_id);
        assert_eq!(replay.lifecycle_memory_id, outcome.lifecycle_memory_id);

        let row: (String, String, Vec<u8>) = sqlx::query_as(
            "SELECT schema_id, authorship_kind::text, payload
               FROM proxima_core.goals
              WHERE goal_id = $1",
        )
        .bind(outcome.goal_id.into_inner())
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(row.0, ProductInitialGoal::SCHEMA_ID);
        assert_eq!(row.1, "User");
        assert_eq!(row.2, expected_key);

        let assigned: (i64,) = sqlx::query_as(
            "SELECT count(*)::bigint
               FROM proxima_core.edges
              WHERE source_kind = 'Goal'::proxima_core.entity_kind
                AND source_goal_id = $1
                AND target_kind = 'Perspective'::proxima_core.entity_kind
                AND target_memory_id = $2",
        )
        .bind(outcome.goal_id.into_inner())
        .bind(target_self.into_inner())
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(assigned.0, 1);

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("typed product GoalWrite surface test failed");
}

#[tokio::test]
async fn engine_goalwrite_conflicts_on_same_request_id_with_changed_side_effects() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let (pg, owner, target_self, engine, authz) = boot_registered(&url).await?;
        engine
            .create_goal(
                &authz,
                product_request(&owner, target_self, "Practice every weekday."),
            )
            .await?;

        let changed_body = engine
            .create_goal(
                &authz,
                product_request(&owner, target_self, "Practice on weekends too."),
            )
            .await
            .expect_err("same request id with changed body conflicts");
        assert_idempotency_conflict(&changed_body);

        let other_self = insert_self(&pg, &owner).await?;
        let changed_self = engine
            .create_goal(
                &authz,
                product_request(&owner, other_self, "Practice every weekday."),
            )
            .await
            .expect_err("same request id with changed Self assignment conflicts");
        assert_idempotency_conflict(&changed_self);

        let evidence = insert_evidence(&pg, &owner).await?;
        let changed_evidence = engine
            .create_goal(
                &authz,
                product_request(&owner, target_self, "Practice every weekday.")
                    .with_evidence(vec![GoalEvidenceRef::new(evidence)]),
            )
            .await
            .expect_err("same request id with changed evidence conflicts");
        assert_idempotency_conflict(&changed_evidence);

        let changed_author_self = engine
            .create_goal(
                &authz,
                product_request(&owner, target_self, "Practice every weekday.")
                    .with_author_self_perspective_id(Some(target_self)),
            )
            .await
            .expect_err("same request id with changed author Self conflicts");
        assert_idempotency_conflict(&changed_author_self);

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("typed product GoalWrite conflict test failed");
}

#[tokio::test]
async fn engine_goalwrite_rejects_duplicate_evidence_before_write() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let (pg, owner, target_self, engine, authz) = boot_registered(&url).await?;
        let evidence = insert_evidence(&pg, &owner).await?;
        let err = engine
            .create_goal(
                &authz,
                product_request(&owner, target_self, "Practice every weekday.").with_evidence(
                    vec![
                        GoalEvidenceRef::new(evidence),
                        GoalEvidenceRef::new(evidence),
                    ],
                ),
            )
            .await
            .expect_err("duplicate evidence must not create ambiguous replay bodies");
        assert_eq!(err.code, ErrorCode::InvalidArgument);
        assert!(err.message.contains("duplicate goal evidence"));

        let goal_count: (i64,) = sqlx::query_as("SELECT count(*)::bigint FROM proxima_core.goals")
            .fetch_one(pg.pool_for_tests())
            .await?;
        assert_eq!(goal_count.0, 0);

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("typed product GoalWrite duplicate-evidence test failed");
}

#[tokio::test]
async fn engine_goalwrite_requires_readable_evidence_before_write() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let (pg, owner, target_self, engine, _) = boot_registered(&url).await?;
        let evidence_owner = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
        let evidence = insert_evidence(&pg, &evidence_owner).await?;
        let subject = UserId::new(Uuid::now_v7());
        let no_read_authz = AuthzContext::for_subject_with_role(
            subject,
            [(owner, Role::admin())],
            AuthPath::System,
        );

        let err = engine
            .create_goal(
                &no_read_authz,
                product_request(&owner, target_self, "Use readable evidence.")
                    .with_evidence(vec![GoalEvidenceRef::new(evidence)]),
            )
            .await
            .expect_err("unreadable evidence rejects before write");
        assert_eq!(err.code, ErrorCode::Forbidden);
        assert_no_goal_rows(&pg).await?;

        let readable_authz = AuthzContext::for_subject_with_role(
            subject,
            [(owner, Role::admin()), (evidence_owner, Role::viewer())],
            AuthPath::System,
        );
        let outcome = engine
            .create_goal(
                &readable_authz,
                product_request(&owner, target_self, "Use readable evidence.")
                    .with_evidence(vec![GoalEvidenceRef::new(evidence)]),
            )
            .await?;
        assert!(!outcome.edge_ids.is_empty());

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("typed product GoalWrite evidence-read test failed");
}

#[tokio::test]
async fn engine_goalwrite_rejects_unknown_wake_trigger_schema_before_write() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let (pg, owner, target_self, engine, authz) = boot_registered(&url).await?;
        let request = product_request(&owner, target_self, "Practice every weekday.").with_wake(
            Some(wake_config(
                GoalWakeTrigger::FactSchema {
                    schema_id: SchemaId::new("test/missing-fact-v1".into()),
                    schema_version: SchemaVersion::new(1),
                },
                &[],
            )),
        );

        let err = engine
            .create_goal(&authz, request)
            .await
            .expect_err("unknown wake trigger Fact schema rejects before write");
        assert_eq!(err.code, ErrorCode::UnknownSchema);
        assert!(err.message.contains("test/missing-fact-v1"));
        assert_no_goal_rows(&pg).await?;
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("wake trigger schema validation test failed");
}

#[tokio::test]
async fn engine_goalwrite_rejects_non_fact_wake_trigger_memory_before_write() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let (pg, owner, target_self, engine, authz) = boot_registered(&url).await?;
        let request = product_request(&owner, target_self, "Practice every weekday.").with_wake(
            Some(wake_config(
                GoalWakeTrigger::FactMemory {
                    memory_id: target_self,
                },
                &[],
            )),
        );

        let err = engine
            .create_goal(&authz, request)
            .await
            .expect_err("non-Fact wake trigger memory rejects before write");
        assert_eq!(err.code, ErrorCode::InvalidArgument);
        assert!(err.message.contains("wake_trigger"));
        assert_no_goal_rows(&pg).await?;
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("wake trigger memory validation test failed");
}

#[tokio::test]
async fn engine_goalwrite_rejects_unreadable_wake_hard_memory_before_write() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let (pg, owner, target_self, engine, authz) = boot_registered(&url).await?;
        let missing_hard_memory = MemoryId::new(Uuid::now_v7());
        let request = product_request(&owner, target_self, "Practice every weekday.").with_wake(
            Some(wake_config(
                GoalWakeTrigger::FactSchema {
                    schema_id: SchemaId::new("core/agent-note-v1".into()),
                    schema_version: SchemaVersion::new(1),
                },
                &[missing_hard_memory],
            )),
        );

        let err = engine
            .create_goal(&authz, request)
            .await
            .expect_err("missing hard memory rejects before write");
        assert_eq!(err.code, ErrorCode::Forbidden);
        assert_no_goal_rows(&pg).await?;
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("wake hard-memory validation test failed");
}

#[tokio::test]
async fn engine_goalwrite_rejects_unregistered_goal_schema() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let target_self = insert_self(&pg, &owner).await?;
        let engine =
            Engine::compose_or_panic_for_tests(Arc::new(pg).storage_ports(), |_registry| {});
        let authz = AuthzContext::single_owner(&owner, AuthPath::System);

        let err = engine
            .create_goal(
                &authz,
                product_request(&owner, target_self, "Practice every weekday."),
            )
            .await
            .expect_err("unregistered product GoalPayload is rejected before storage insert");

        assert_eq!(err.code, ErrorCode::UnknownSchema);
        assert!(err.message.contains(ProductInitialGoal::SCHEMA_ID));

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("typed product GoalWrite schema-validation test failed");
}

#[tokio::test]
async fn engine_goalwrite_rejects_unauthorized_callers_before_write() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let (pg, owner, target_self, engine, _authz) = boot_registered(&url).await?;

        // Cross-owner: a context scoped to `owner` cannot create a Goal for a
        // different principal (the `can_access_principal` branch), even with
        // full graph_write capabilities.
        let other_owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let owner_authz = AuthzContext::for_subject_with_role(
            UserId::new(Uuid::now_v7()),
            [(owner, Role::admin())],
            AuthPath::System,
        );
        let cross_owner = engine
            .create_goal(
                &owner_authz,
                product_request(&other_owner, target_self, "Practice every weekday."),
            )
            .await
            .expect_err("cross-owner principal is rejected");
        assert_eq!(cross_owner.code, ErrorCode::Forbidden);

        // Owner-space authorization: a Viewer grant is not enough for
        // GoalWrite, which requires Editor before reaching storage.
        let no_memory_write = viewer_without_memory_write(&pg, &owner).await;
        let no_memory_write_err = engine
            .create_goal(
                &no_memory_write,
                product_request(&owner, target_self, "Practice every weekday."),
            )
            .await
            .expect_err("explicit grant without memory.write is rejected");
        assert_eq!(no_memory_write_err.code, ErrorCode::Forbidden);

        // Fail-closed: the zero-capability denied context (the unauthenticated
        // posture) carries no graph_write role and is rejected.
        let denied = AuthzContext::denied_for_owner(&owner);
        let denied_err = engine
            .create_goal(
                &denied,
                product_request(&owner, target_self, "Practice every weekday."),
            )
            .await
            .expect_err("denied (no graph_write) context is rejected");
        assert_eq!(denied_err.code, ErrorCode::Forbidden);

        // Authorization is enforced before any storage write: no Goal row exists.
        let goal_count: (i64,) = sqlx::query_as("SELECT count(*)::bigint FROM proxima_core.goals")
            .fetch_one(pg.pool_for_tests())
            .await?;
        assert_eq!(goal_count.0, 0);

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("typed product GoalWrite authz test failed");
}
