mod common;

use common::{drop_db, fresh_pg, owner_fixture};
use proxima_core::{
    BindInferenceTierRequest, InferenceTargetConfig, MistralChatConfig, ModelTier,
    RegisterInferenceTargetRequest, RemoveInferenceTargetRequest, Storage,
};

fn request(owner: proxima_core::Owner, target_ref: &str) -> RegisterInferenceTargetRequest {
    RegisterInferenceTargetRequest {
        owner,
        target_ref: target_ref.into(),
        config: InferenceTargetConfig::MistralChat(MistralChatConfig {
            base_url: "http://127.0.0.1:9".into(),
            model_id: "test-model".into(),
            api_key_env: "PATH".into(),
            temperature: None,
            max_completion_tokens: None,
            reasoning_effort: None,
        }),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn remove_inference_target_succeeds_when_unreferenced() {
    let Some((pg, db)) = fresh_pg().await else {
        return;
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        let owner = owner_fixture();
        pg.register_inference_target(&request(owner.clone(), "tmp"))
            .await?;

        let out = pg
            .remove_inference_target(&RemoveInferenceTargetRequest {
                owner: owner.clone(),
                target_ref: "tmp".into(),
            })
            .await?;
        assert!(!out.idempotent_replay);
        assert!(pg.list_inference_targets(&owner).await?.is_empty());
        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db).await;
    result.expect("remove inference target succeeds");
}

#[tokio::test(flavor = "multi_thread")]
async fn remove_inference_target_idempotent_when_absent() {
    let Some((pg, db)) = fresh_pg().await else {
        return;
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        let owner = owner_fixture();
        let out = pg
            .remove_inference_target(&RemoveInferenceTargetRequest {
                owner,
                target_ref: "nonexistent".into(),
            })
            .await?;
        assert!(out.idempotent_replay);
        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db).await;
    result.expect("remove inference target is idempotent");
}

#[tokio::test(flavor = "multi_thread")]
async fn remove_inference_target_blocked_by_tier_binding() {
    let Some((pg, db)) = fresh_pg().await else {
        return;
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        let owner = owner_fixture();
        pg.register_inference_target(&request(owner.clone(), "fast-target"))
            .await?;
        pg.bind_inference_tier(&BindInferenceTierRequest {
            owner: owner.clone(),
            tier: ModelTier::Fast,
            target_ref: "fast-target".into(),
        })
        .await?;

        let err = pg
            .remove_inference_target(&RemoveInferenceTargetRequest {
                owner,
                target_ref: "fast-target".into(),
            })
            .await
            .expect_err("should refuse target in use");
        assert!(format!("{err}").to_lowercase().contains("in use"));
        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db).await;
    result.expect("remove inference target blocks references");
}
