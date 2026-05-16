//! Workspace review-loop wake wiring.
//!
//! No bootstrap personality is created here. The test mirrors the
//! operator/MCP setup path: instantiate personalities, attach explicit
//! wake entries, and prove the Code registry accepts the review-loop
//! workspace triggers.

use proxima_code::{WorkspaceDecisionV1, WorkspaceReviewV1, WorkspaceRunV1, build_engine};
use proxima_core::auth::NoAuth;
use proxima_core::storage::Storage;
use proxima_core::{
    BindInferenceTierRequest, Credentials, FactPayload, InferenceTargetConfig, MistralChatConfig,
    ModelTier, OrgId, Owner, Principal, RegisterInferenceTargetRequest, SetWakeEntriesRequest,
    UserId, WakeEntryAuthoredBy, WakeEntryDraft, WakeEntryTriggerKind, WakeExecutionMode,
};
use proxima_storage_pg::PgStorage;
use sqlx::{Connection, Executor, PgConnection, Row};
use uuid::Uuid;

const ADMIN_URL: &str = "postgres://proxima:proxima@localhost/postgres";

async fn create_db(name: &str) -> Result<(), sqlx::Error> {
    let admin = std::env::var("PROXIMA_TEST_PG_URL").unwrap_or_else(|_| ADMIN_URL.into());
    let mut conn = PgConnection::connect(&admin).await?;
    conn.execute(format!("CREATE DATABASE \"{name}\"").as_str())
        .await?;
    conn.close().await?;
    Ok(())
}

async fn drop_db(name: &str) -> Result<(), sqlx::Error> {
    let admin = std::env::var("PROXIMA_TEST_PG_URL").unwrap_or_else(|_| ADMIN_URL.into());
    let mut conn = PgConnection::connect(&admin).await?;
    conn.execute(format!("DROP DATABASE IF EXISTS \"{name}\"").as_str())
        .await?;
    conn.close().await?;
    Ok(())
}

fn db_url(db_name: &str) -> String {
    let admin = std::env::var("PROXIMA_TEST_PG_URL").unwrap_or_else(|_| ADMIN_URL.into());
    match admin.rfind('/') {
        Some(idx) => format!("{}/{}", &admin[..idx], db_name),
        None => format!("{admin}/{db_name}"),
    }
}

async fn migrated_db() -> Option<(String, PgStorage)> {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let pg = PgStorage::connect(&db_url(&db_name))
        .await
        .expect("connect test db");
    if let Err(err) = async {
        pg.run_migrations().await?;
        proxima_code::migrator().run(pg.pool()).await?;
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await
    {
        drop(pg);
        let _ = drop_db(&db_name).await;
        panic!("migration failed: {err}");
    }
    Some((db_name, pg))
}

fn test_owner() -> Owner {
    Owner {
        principal: Principal::User(UserId::new(Uuid::now_v7())),
        org_id: OrgId::new(Uuid::now_v7()),
    }
}

fn workspace_wake(
    personality_instance_id: proxima_core::PersonalityInstanceId,
    trigger_id: &str,
    label: &str,
    workspace_tool_palette: Vec<String>,
) -> Result<WakeEntryDraft, proxima_core::ProtocolError> {
    let mut wake = WakeEntryDraft::new(
        Uuid::now_v7(),
        personality_instance_id,
        WakeEntryTriggerKind::OnMemory,
        trigger_id,
        label,
        WakeEntryAuthoredBy::Other,
        1000,
        ModelTier::Standard,
        None,
        Vec::new(),
        0,
    )?;
    wake.execution_mode = WakeExecutionMode::Workspace;
    wake.workspace_tool_palette = workspace_tool_palette;
    Ok(wake)
}

#[tokio::test(flavor = "multi_thread")]
#[allow(clippy::too_many_lines)]
async fn workspace_review_loop_wake_entries_validate() -> Result<(), Box<dyn std::error::Error>> {
    let Some((db_name, pg)) = migrated_db().await else {
        return Ok(());
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = test_owner();
        let engine = build_engine(
            pg.clone(),
            Box::new(NoAuth::new(owner.principal.clone(), owner.clone())),
        );

        pg.register_inference_target(&RegisterInferenceTargetRequest {
            owner: owner.clone(),
            target_ref: "test/review-loop".into(),
            config: InferenceTargetConfig::MistralChat(MistralChatConfig {
                base_url: "http://127.0.0.1:9".into(),
                model_id: "test-model".into(),
                api_key_env: "PATH".into(),
                temperature: None,
                max_completion_tokens: None,
            }),
        })
        .await?;
        pg.bind_inference_tier(&BindInferenceTierRequest {
            owner: owner.clone(),
            tier: ModelTier::Standard,
            target_ref: "test/review-loop".into(),
        })
        .await?;

        let verifier = engine
            .instantiate_personality(proxima_core::InstantiatePersonalityRequest {
                owner: owner.clone(),
                display_name: "Workspace Verifier".into(),
                purpose: "Review completed Code workspace runs".into(),
            })
            .await?;
        let correction_planner = engine
            .instantiate_personality(proxima_core::InstantiatePersonalityRequest {
                owner: owner.clone(),
                display_name: "Workspace Correction Planner".into(),
                purpose: "Plan corrections for rejected workspace runs".into(),
            })
            .await?;

        let verifier_wake = workspace_wake(
            verifier.instance_id,
            WorkspaceRunV1::SCHEMA_ID,
            "verify-workspace-run",
            vec!["proxima-workspace/shell".into()],
        )?;
        let verifier_out = engine
            .set_wake_entries(
                &Credentials::None,
                &SetWakeEntriesRequest {
                    owner: owner.clone(),
                    personality_instance_id: verifier.instance_id,
                    entries: vec![verifier_wake.clone()],
                },
            )
            .await?;
        assert_eq!(verifier_out.active_entries, 1);

        let review_wake = workspace_wake(
            correction_planner.instance_id,
            WorkspaceReviewV1::SCHEMA_ID,
            "plan-workspace-review-correction",
            Vec::new(),
        )?;
        let decision_wake = workspace_wake(
            correction_planner.instance_id,
            WorkspaceDecisionV1::SCHEMA_ID,
            "plan-workspace-decision-correction",
            Vec::new(),
        )?;
        let correction_out = engine
            .set_wake_entries(
                &Credentials::None,
                &SetWakeEntriesRequest {
                    owner: owner.clone(),
                    personality_instance_id: correction_planner.instance_id,
                    entries: vec![review_wake.clone(), decision_wake.clone()],
                },
            )
            .await?;
        assert_eq!(correction_out.active_entries, 2);

        let rows = sqlx::query(
            "SELECT trigger_id, execution_mode, workspace_tool_palette
             FROM proxima_core.personality_wake_entries
             WHERE personality_instance_id IN ($1, $2)
               AND tombstoned_at IS NULL
             ORDER BY trigger_id",
        )
        .bind(verifier.instance_id.into_inner())
        .bind(correction_planner.instance_id.into_inner())
        .fetch_all(pg.pool())
        .await?;
        assert_eq!(rows.len(), 3);

        let persisted: Vec<(String, WakeExecutionMode, Vec<String>)> = rows
            .into_iter()
            .map(|row| {
                Ok::<_, sqlx::Error>((
                    row.try_get("trigger_id")?,
                    row.try_get("execution_mode")?,
                    row.try_get("workspace_tool_palette")?,
                ))
            })
            .collect::<Result<_, _>>()?;
        assert!(persisted.iter().any(|(trigger, mode, tools)| {
            trigger == WorkspaceRunV1::SCHEMA_ID
                && *mode == WakeExecutionMode::Workspace
                && tools == &vec!["proxima-workspace/shell".to_string()]
        }));
        assert!(persisted.iter().any(|(trigger, mode, tools)| {
            trigger == WorkspaceReviewV1::SCHEMA_ID
                && *mode == WakeExecutionMode::Workspace
                && tools.is_empty()
        }));
        assert!(persisted.iter().any(|(trigger, mode, tools)| {
            trigger == WorkspaceDecisionV1::SCHEMA_ID
                && *mode == WakeExecutionMode::Workspace
                && tools.is_empty()
        }));

        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db_name).await;
    result
}
