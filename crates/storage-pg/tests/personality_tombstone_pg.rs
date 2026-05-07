//! Tombstoned wake configs disappear from default listings, never wake
//! again, refuse SetWakeConfig restoration, and the operation is
//! idempotent on the natural key. Cognitive history (memories, the
//! self-Perspective) is untouched.

mod common;

use std::sync::Arc;

use common::{drop_db, fresh_pg, owner_fixture};
use proxima_core::auth::NoAuth;
use proxima_core::engine::Engine;
use proxima_core::error::ErrorCode;
use proxima_core::personality::{
    InstantiatePersonalityRequest, PersonalityInstanceId, SetWakeConfigRequest,
    TombstonePersonalityRequest, WakeFilter,
};
use proxima_core::verbs::query::MemoryStore;
use proxima_core::verbs::schema::FlavorRegistryFrozen;
use proxima_core::{Principal, SchemaId, Storage, StorageError};

async fn apply_self_sidecar(pool: &sqlx::PgPool) -> sqlx::Result<()> {
    sqlx::Executor::execute(
        pool,
        "CREATE SCHEMA IF NOT EXISTS proxima_test; \
         CREATE TABLE IF NOT EXISTS proxima_test.personality_self_v1 ( \
             memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id), \
             display_name text NOT NULL, \
             purpose text NOT NULL \
         );",
    )
    .await
    .map(|_| ())
}

fn self_draft(name: &str) -> proxima_core::PersonalitySelfDraft {
    proxima_core::PersonalitySelfDraft {
        schema_id: SchemaId::new("proxima-test/self-v1".into()),
        schema_version: proxima_core::SchemaVersion::new(1),
        text: name.into(),
        typed_payload: serde_json::json!({ "display_name": name, "purpose": "test" }),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn tombstone_hides_instance_and_keeps_history() {
    let Some((pg, db)) = fresh_pg().await else {
        return;
    };
    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        apply_self_sidecar(pg.pool()).await?;
        let owner = owner_fixture();
        let req = InstantiatePersonalityRequest {
            owner: owner.clone(),
            personality_type_id: "proxima-test/personality-v1".into(),
            payload_overrides: None,
        };
        let filters = vec![WakeFilter::on_memory(SchemaId::new(
            "proxima-test/fact-v1".into(),
        ))];
        let created = pg
            .instantiate_personality(
                &req,
                &self_draft("Engineer A"),
                "proxima_test.personality_self_v1",
                &filters,
            )
            .await?;

        let out = pg
            .tombstone_personality(&TombstonePersonalityRequest {
                owner: owner.clone(),
                personality_type_id: req.personality_type_id.clone(),
                personality_instance_id: created.instance_id,
            })
            .await?;
        assert_eq!(out.status, "tombstoned");
        assert!(!out.idempotent_replay);

        assert!(
            pg.list_personality_instances(&owner, None, false)
                .await?
                .is_empty(),
            "default listing must hide tombstoned rows"
        );
        let all = pg
            .list_personality_instances(&owner, None, true)
            .await?;
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].status, "tombstoned");
        assert!(
            pg.list_active_wake_configs().await?.is_empty(),
            "dispatcher selection must skip tombstoned rows"
        );

        let self_memory_id = all[0].current_self_perspective_memory_id;
        let memory_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM proxima_core.memories WHERE memory_id = $1",
        )
        .bind(self_memory_id.into_inner())
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(
            memory_count, 1,
            "tombstoning must not delete the self-Perspective memory row"
        );
        Ok(())
    }
    .await;
    drop(pg);
    let _ = drop_db(&db).await;
    result.expect("tombstone behavior failed");
}

#[tokio::test(flavor = "multi_thread")]
async fn tombstone_is_idempotent_and_set_wake_config_cannot_restore() {
    let Some((pg, db)) = fresh_pg().await else {
        return;
    };
    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        apply_self_sidecar(pg.pool()).await?;
        let owner = owner_fixture();
        let req = InstantiatePersonalityRequest {
            owner: owner.clone(),
            personality_type_id: "proxima-test/personality-v1".into(),
            payload_overrides: None,
        };
        let filters = vec![WakeFilter::on_memory(SchemaId::new(
            "proxima-test/fact-v1".into(),
        ))];
        let created = pg
            .instantiate_personality(
                &req,
                &self_draft("Engineer A"),
                "proxima_test.personality_self_v1",
                &filters,
            )
            .await?;
        let tombstone = TombstonePersonalityRequest {
            owner: owner.clone(),
            personality_type_id: req.personality_type_id.clone(),
            personality_instance_id: created.instance_id,
        };

        let first = pg.tombstone_personality(&tombstone).await?;
        assert!(!first.idempotent_replay);
        let second = pg.tombstone_personality(&tombstone).await?;
        assert!(second.idempotent_replay);

        let restore = pg
            .set_wake_config(&SetWakeConfigRequest {
                owner,
                personality_type_id: req.personality_type_id,
                personality_instance_id: created.instance_id,
                wake_filters: filters,
            })
            .await
            .expect_err("tombstone cannot be restored by SetWakeConfig");
        assert!(matches!(restore, StorageError::NotFound));
        Ok(())
    }
    .await;
    drop(pg);
    let _ = drop_db(&db).await;
    result.expect("tombstone idempotency failed");
}

/// Engine surfaces `StorageError::NotFound` from
/// `tombstone_personality` and `set_wake_config` as
/// `ProtocolError { code: NotFound, .. }` — not Internal — so Tauri /
/// gRPC clients can distinguish stale UI from server failure.
#[tokio::test(flavor = "multi_thread")]
async fn engine_maps_storage_not_found_to_protocol_not_found() {
    let Some((pg, db)) = fresh_pg().await else {
        return;
    };
    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        let owner = owner_fixture();
        let principal: Principal = owner.principal.clone();
        let engine = Engine::new(
            FlavorRegistryFrozen::new(),
            MemoryStore::new(),
            Box::new(NoAuth::new(principal, owner.clone())),
        )
        .with_storage(Arc::new(pg));

        let missing_instance = PersonalityInstanceId::new(uuid::Uuid::now_v7());

        let tombstone_err = engine
            .tombstone_personality(TombstonePersonalityRequest {
                owner: owner.clone(),
                personality_type_id: "proxima-test/missing-v1".into(),
                personality_instance_id: missing_instance,
            })
            .await
            .expect_err("tombstoning a missing instance must error");
        assert_eq!(
            tombstone_err.code,
            ErrorCode::NotFound,
            "missing instance must surface NotFound, not Internal"
        );

        let set_wake_err = engine
            .set_wake_config(SetWakeConfigRequest {
                owner,
                personality_type_id: "proxima-test/missing-v1".into(),
                personality_instance_id: missing_instance,
                wake_filters: vec![WakeFilter::on_memory(SchemaId::new(
                    "proxima-test/fact-v1".into(),
                ))],
            })
            .await
            .expect_err("setting wake config on missing instance must error");
        assert_eq!(
            set_wake_err.code,
            ErrorCode::NotFound,
            "missing instance must surface NotFound, not Internal"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db).await;
    result.expect("engine NotFound mapping failed");
}
