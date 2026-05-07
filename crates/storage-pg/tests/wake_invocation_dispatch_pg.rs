//! Phase 1d: Wake invocation dispatch columns survive INSERT/UPDATE
//! roundtrip via the storage trait.

mod common;

use common::{drop_db, fresh_pg, owner_fixture};
use proxima_core::personality::{
    InstantiatePersonalityRequest, PersonalityInstanceId, PersonalitySelfDraft,
    SetWakeEntriesRequest, WakeEntryAuthoredBy, WakeEntryDraft, WakeEntryTriggerKind,
    WakeInvocationFinalize, WakeInvocationStart, WakeInvocationStatus,
};
use proxima_core::storage::Storage;
use proxima_core::{ModelTier, Owner, Principal, SchemaId, SchemaVersion};
use sqlx::Executor;
use uuid::Uuid;

#[derive(Debug)]
struct WakeInvocationDispatchRow {
    wake_token: Option<Uuid>,
    recipe_sha256: Option<String>,
    resolved_inference_target_ref: Option<String>,
    failure_reason: Option<String>,
    status: WakeInvocationStatus,
}

async fn apply_self_sidecar(pool: &sqlx::PgPool) -> sqlx::Result<()> {
    pool.execute(
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

fn self_draft(display_name: &str) -> PersonalitySelfDraft {
    PersonalitySelfDraft {
        schema_id: SchemaId::new("proxima-test/self-v1".into()),
        schema_version: SchemaVersion::new(1),
        text: display_name.into(),
        typed_payload: serde_json::json!({
            "display_name": display_name,
            "purpose": "exercise wake invocation dispatch columns",
        }),
    }
}

async fn seed_personality_with_entry(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
) -> Result<(PersonalityInstanceId, Uuid), Box<dyn std::error::Error>> {
    apply_self_sidecar(pg.pool()).await?;
    let response = pg
        .instantiate_personality(
            &InstantiatePersonalityRequest {
                owner: owner.clone(),
                personality_type_id: "proxima-test/personality-v1".into(),
                payload_overrides: None,
            },
            &self_draft("Engineer A"),
            "proxima_test.personality_self_v1",
        )
        .await?;
    let entry = WakeEntryDraft::new(
        Uuid::now_v7(),
        response.instance_id,
        WakeEntryTriggerKind::OnMemory,
        "proxima-test/fact-v1",
        "on_test_fact",
        WakeEntryAuthoredBy::Any,
        1000,
        "bundled:proxima-test/personality-v1",
        ModelTier::Fast,
        Some("local-cli:goose".to_string()),
        vec!["core/query".to_string()],
        4,
    )
    .expect("valid wake entry");
    let wake_entry_id = entry.wake_entry_id;
    pg.set_wake_entries(&SetWakeEntriesRequest {
        owner: owner.clone(),
        personality_instance_id: response.instance_id,
        entries: vec![entry],
    })
    .await?;
    Ok((response.instance_id, wake_entry_id))
}

async fn fetch_wake_invocation(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
    instance_id: PersonalityInstanceId,
    wake_entry_id: Uuid,
    change_event_seq: Uuid,
) -> Result<WakeInvocationDispatchRow, Box<dyn std::error::Error>> {
    let principal_id = match owner.principal {
        Principal::User(id) => id.into_inner(),
        Principal::Group(id) => id.into_inner(),
    };
    let (wake_token, recipe_sha256, resolved_inference_target_ref, failure_reason, status): (
        Option<Uuid>,
        Option<String>,
        Option<String>,
        Option<String>,
        String,
    ) = sqlx::query_as(
        "SELECT wake_token, recipe_sha256, resolved_inference_target_ref,
                failure_reason, status
         FROM proxima_core.personality_wake_invocations
         WHERE owner_principal_id = $1
           AND owner_org_id = $2
           AND personality_instance_id = $3
           AND wake_entry_id = $4
           AND change_event_seq = $5",
    )
    .bind(principal_id)
    .bind(owner.org_id.into_inner())
    .bind(instance_id.into_inner())
    .bind(wake_entry_id)
    .bind(change_event_seq)
    .fetch_one(pg.pool())
    .await?;
    Ok(WakeInvocationDispatchRow {
        wake_token,
        recipe_sha256,
        resolved_inference_target_ref,
        failure_reason,
        status: match status.as_str() {
            "succeeded" => WakeInvocationStatus::Succeeded,
            "truncated" => WakeInvocationStatus::Truncated,
            "failed" => WakeInvocationStatus::Failed,
            _ => WakeInvocationStatus::Running,
        },
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn wake_invocation_carries_dispatch_columns() {
    let Some((pg, db)) = fresh_pg().await else {
        return;
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        let owner = owner_fixture();
        let (instance_id, wake_entry_id) = seed_personality_with_entry(&pg, &owner).await?;
        let change_event_seq = Uuid::now_v7();

        let wake_token = Uuid::new_v4();
        let recipe_sha256 = "deadbeef".repeat(8);
        let resolved_target = "default-standard";

        let start = WakeInvocationStart {
            owner: owner.clone(),
            personality_instance_id: instance_id,
            wake_entry_id,
            change_event_seq,
            wake_token,
            recipe_sha256: recipe_sha256.clone(),
            resolved_inference_target_ref: resolved_target.to_string(),
        };
        pg.start_wake_invocation(&start).await.expect("start ok");

        let finalize = WakeInvocationFinalize {
            owner: owner.clone(),
            personality_instance_id: instance_id,
            wake_entry_id,
            change_event_seq,
            status: WakeInvocationStatus::Failed,
            turn_count: None,
            cost_usd: None,
            failure_reason: Some("workspace_mode_not_yet_implemented".to_string()),
        };
        pg.finalize_wake_invocation(&finalize)
            .await
            .expect("finalize ok");

        let row = fetch_wake_invocation(&pg, &owner, instance_id, wake_entry_id, change_event_seq)
            .await?;
        assert_eq!(row.wake_token, Some(wake_token));
        assert_eq!(row.recipe_sha256.as_deref(), Some(recipe_sha256.as_str()));
        assert_eq!(
            row.resolved_inference_target_ref.as_deref(),
            Some(resolved_target)
        );
        assert_eq!(
            row.failure_reason.as_deref(),
            Some("workspace_mode_not_yet_implemented")
        );
        assert!(matches!(row.status, WakeInvocationStatus::Failed));
        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db).await;
    result.expect("wake invocation dispatch columns roundtrip failed");
}
