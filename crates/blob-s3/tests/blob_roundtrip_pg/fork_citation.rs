use super::*;

use proxiima_core::engine::Engine;
use proxiima_core::memory::payloads::AgentDerivationV1;
use proxiima_core::storage_ports::CitedBlobReadPort;
use proxiima_core::test_fixtures::authenticated_context;
use proxiima_core::verbs::goal_write::{
    GoalAuthorship, GoalCreateRequest, GoalTopologyWrite, IdempotencyKey,
};
use proxiima_core::{
    AuthPath, AuthzContext, DerivationIdentity, DerivedMemory, ForkMemoryRequest,
    GoalAudienceGrantInput, GoalPublicationInput, InputContractId, MemoryDestination, MemoryId,
    MemoryTarget, OperatorId, OwnerId, PublicationUse, Role, SeriesHandle, SimpleTextGoalV1,
    SpiiceRegistry, UserId,
};
use std::num::NonZeroU64;
use std::sync::Arc;
use uuid::Uuid;

/// The mounted-object path used by an owner transfer is the same persistence
/// contract the fork path reuses: a destination-owned blob/upload row points
/// at the immutable source object instead of copying bytes.
#[tokio::test(flavor = "multi_thread")]
async fn mounted_citation_survives_source_owner_erase() {
    if !S3RuntimeConfig::present_in_env() {
        eprintln!("skipped: PROXIIMA_S3_* unset");
        return;
    }

    let (pg, db_name) = fresh_storage().await;
    let pool = pg.pool_for_tests().clone();
    let config = s3_config_for_dev();
    let store = CitedBlobStore::new(
        pg.clone_pool_for_backend(),
        config.clone(),
        pg.platform_scope_for_host(),
    )
    .expect("valid S3 config");
    let source = owner_fixture();
    let source_auth = authenticated_context(AuthzContext::for_subject(
        UserId::new(source.into_inner()),
        AuthPath::HostBearer,
    ));
    let destination = OwnerId::from(UserId::new(Uuid::now_v7()));
    let engine = Engine::new(SpiiceRegistry::new().freeze_or_panic_for_tests())
        .with_storage_ports(Arc::new(pg.clone()).storage_ports());
    let source_data =
        seed_cited_source(&pg, &store, &config, &pool, &engine, &source_auth, source).await;
    let (destination_auth, _goal) =
        prepare_fork(&engine, &source_auth, destination, source_data.memory).await;
    add_private_same_body_upload(&pg, &store, &config, &pool, &source_auth, source).await;
    let fork = execute_fork(&engine, &destination_auth, destination, source_data.memory).await;
    let destination_blob = assert_mounted_citation(
        &pool,
        &store,
        &source_data,
        fork.memory_id,
        destination,
        &destination_auth,
    )
    .await;
    erase_owner_and_drain(&pg, &store, source).await;
    assert_destination_readable(&store, destination_blob, destination, &destination_auth).await;
    erase_owner_and_drain(&pg, &store, destination).await;
    assert_object_purged(&config, &source_data.key).await;

    drop(pool);
    drop_db(&db_name).await.expect("drop test database");
}

struct SourceData {
    memory: MemoryId,
    blob: Uuid,
    upload: Uuid,
    key: String,
}

async fn seed_cited_source(
    pg: &PgStorage,
    store: &CitedBlobStore,
    config: &S3RuntimeConfig,
    pool: &sqlx::PgPool,
    engine: &Engine,
    auth: &AuthzContext,
    owner: OwnerId,
) -> SourceData {
    let (blob_text, key) = complete_upload_fixture(pg, store, config, pool, auth, owner)
        .await
        .expect("completed citation");
    let blob = Uuid::parse_str(&blob_text).expect("blob id");
    let upload = sqlx::query_scalar(
        "SELECT upload_id FROM proxiima_core.blob_uploads
          WHERE blob_id = $1 AND owner_id = $2 AND filename = 'genuine.pdf'",
    )
    .bind(blob)
    .bind(owner.into_inner())
    .fetch_one(pool)
    .await
    .expect("source upload");
    let fact: Uuid = sqlx::query_scalar(
        "SELECT t FROM proxiima_core.memory WHERE owner_id = $1 AND blob_id = $2 ORDER BY t LIMIT 1",
    )
    .bind(owner.into_inner())
    .bind(blob)
    .fetch_one(pool)
    .await
    .expect("source Fact");
    let uncited = engine
        .derive_memory(
            auth,
            DerivedMemory::abstraction(
                MemoryTarget::Series(SeriesHandle::new(Uuid::now_v7())),
                owner,
                "cited abstraction",
                AgentDerivationV1 {
                    title: "cited abstraction".into(),
                    body: "source body".into(),
                    tags: Vec::new(),
                    idempotency_key: None,
                    source_memory_ids: vec![fact],
                    model_id: "fork-s3".into(),
                    client_name: "fork-s3".into(),
                    client_version: "1".into(),
                },
                [MemoryId::new(fact)],
                DerivationIdentity::new(
                    OperatorId::new(Uuid::now_v7()),
                    InputContractId::new(Uuid::now_v7()),
                ),
            )
            .expect("typed abstraction"),
        )
        .await
        .expect("source abstraction")
        .memory_id;
    let memory =
        proxiima_storage_pg::test_fixtures::cited_abstraction_from_template(pool, uncited, blob)
            .await
            .expect("cited abstraction fixture");
    SourceData {
        memory,
        blob,
        upload,
        key,
    }
}

async fn prepare_fork(
    engine: &Engine,
    source_auth: &AuthzContext,
    destination: OwnerId,
    memory: MemoryId,
) -> (AuthzContext, proxiima_core::GoalHandle) {
    let goal = engine
        .create_goal(
            source_auth,
            GoalCreateRequest {
                owner: OwnerId::from(source_auth.subject().expect("source subject")),
                topology: GoalTopologyWrite::new(Vec::new(), Vec::new(), Vec::new())
                    .expect("empty topology"),
                title: "fork citation".into(),
                text: "share one cited abstraction".into(),
                payload: SimpleTextGoalV1 {},
                request_id: IdempotencyKey::new("fork-s3-goal").expect("goal key"),
                authorship: GoalAuthorship::User,
                author_self_perspective_id: None,
            },
        )
        .await
        .expect("fork goal")
        .goal_handle;
    engine
        .grant_goal_access(
            source_auth,
            &GoalAudienceGrantInput {
                goal,
                subject: destination.as_user(),
                role: Role::editor(),
                expires_at: None,
            },
        )
        .await
        .expect("fork editor grant");
    engine
        .publish_to_goal(
            source_auth,
            &GoalPublicationInput {
                goal,
                memory,
                permitted_use: PublicationUse::Fork,
            },
        )
        .await
        .expect("publish fork source");
    let auth = authenticated_context(AuthzContext::for_subject(
        destination.as_user(),
        AuthPath::HostBearer,
    ));
    (auth, goal)
}

async fn execute_fork(
    engine: &Engine,
    auth: &AuthzContext,
    destination: OwnerId,
    memory: MemoryId,
) -> proxiima_core::ForkMemoryOutcome {
    engine
        .fork_memory(
            auth,
            &ForkMemoryRequest {
                source: memory,
                destination: MemoryDestination::Owner(destination),
                request_id: IdempotencyKey::new("fork-s3-memory").expect("fork key"),
            },
        )
        .await
        .expect("cited abstraction fork")
}

async fn add_private_same_body_upload(
    pg: &PgStorage,
    store: &CitedBlobStore,
    config: &S3RuntimeConfig,
    pool: &sqlx::PgPool,
    auth: &AuthzContext,
    owner: OwnerId,
) {
    let later = store
        .prepare_upload(
            auth,
            CitedBlobUploadPrepareTs {
                owner,
                filename: "private-later.pdf".into(),
                mime: "application/pdf".into(),
                byte_len: 4,
            },
        )
        .await
        .expect("prepare later upload");
    let key: String = sqlx::query_scalar(
        "SELECT object_key FROM proxiima_core.blob_uploads WHERE upload_id = $1",
    )
    .bind(Uuid::parse_str(&later.upload_id).expect("upload id"))
    .fetch_one(pool)
    .await
    .expect("later row");
    put_object_via_sdk(config, &key, b"real").await;
    complete_via_engine(pg, store, auth, owner, &later.upload_id)
        .await
        .expect("complete later upload");
}

async fn assert_mounted_citation(
    pool: &sqlx::PgPool,
    store: &CitedBlobStore,
    source: &SourceData,
    fork_memory: MemoryId,
    destination: OwnerId,
    auth: &AuthzContext,
) -> Uuid {
    let (blob, key, mounted, filename): (Uuid, String, Option<Uuid>, String) = sqlx::query_as(
        "SELECT m.blob_id, u.object_key, u.mounted_from_upload_id, u.filename
           FROM proxiima_core.memory m JOIN proxiima_core.blob_uploads u ON u.blob_id = m.blob_id
          WHERE m.t = $1 AND m.owner_id = $2",
    )
    .bind(fork_memory.into_inner())
    .bind(destination.into_inner())
    .fetch_one(pool)
    .await
    .expect("mounted citation");
    assert_ne!(blob, source.blob);
    assert_eq!(key, source.key);
    assert_eq!(mounted, Some(source.upload));
    assert_eq!(filename, "genuine.pdf");
    let bytes = store
        .collect_verified(
            auth,
            destination,
            blob,
            NonZeroU64::new(1024).expect("limit"),
        )
        .await
        .expect("read mounted citation");
    assert_eq!(bytes.bytes, b"real");
    blob
}

async fn assert_destination_readable(
    store: &CitedBlobStore,
    blob: Uuid,
    destination: OwnerId,
    auth: &AuthzContext,
) {
    let bytes = store
        .collect_verified(
            auth,
            destination,
            blob,
            NonZeroU64::new(1024).expect("limit"),
        )
        .await
        .expect("destination mount remains readable after source erase");
    assert_eq!(bytes.bytes, b"real");
    assert_eq!(bytes.filename, "genuine.pdf");
}

async fn assert_object_purged(config: &S3RuntimeConfig, key: &str) {
    let error = config
        .client()
        .head_object()
        .bucket(&config.bucket)
        .key(key)
        .send()
        .await
        .expect_err("final destination claim must purge the object")
        .into_service_error();
    assert!(
        error.is_not_found(),
        "final destination claim returns NotFound"
    );
}
