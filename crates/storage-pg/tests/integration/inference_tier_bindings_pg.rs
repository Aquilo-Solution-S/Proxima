use crate::common::{drop_db, fresh_pg, owner_fixture};
use proxima_core::{
    BindInferenceTierRequest, InferenceTargetConfig, MistralChatConfig, ModelTier,
    RegisterInferenceTargetRequest, Storage,
};

async fn register(
    pg: &proxima_storage_pg::PgStorage,
    owner: &proxima_core::Owner,
    target_ref: &str,
) -> Result<(), proxima_core::StorageError> {
    pg.register_inference_target(&RegisterInferenceTargetRequest {
        principal: owner.principal.clone(),
        org_id: Some(owner.org_id),
        target_ref: target_ref.into(),
        config: InferenceTargetConfig::MistralChat(MistralChatConfig {
            base_url: "http://127.0.0.1:9".into(),
            model_id: "test-model".into(),
            api_key_env: "PATH".into(),
            temperature: None,
            max_completion_tokens: None,
            reasoning_effort: None,

            context_window_tokens: None,
        }),
    })
    .await
    .map(|_| ())
}

#[tokio::test(flavor = "multi_thread")]
async fn bind_unbind_tier_round_trip() {
    let Some((pg, db)) = fresh_pg().await else {
        return;
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        let owner = owner_fixture();
        register(&pg, &owner, "t1").await?;

        pg.bind_inference_tier(&BindInferenceTierRequest {
            principal: owner.principal.clone(),
            org_id: Some(owner.org_id),
            tier: ModelTier::Standard,
            target_ref: "t1".into(),
        })
        .await?;

        let bindings = pg.list_inference_tier_bindings(&owner).await?;
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].tier, ModelTier::Standard);
        assert_eq!(bindings[0].target_ref, "t1");

        pg.unbind_inference_tier(&owner, ModelTier::Standard)
            .await?;
        assert!(pg.list_inference_tier_bindings(&owner).await?.is_empty());
        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db).await;
    result.expect("bind/unbind inference tier round trip");
}

#[tokio::test(flavor = "multi_thread")]
async fn bind_inference_tier_upserts_existing_binding() {
    let Some((pg, db)) = fresh_pg().await else {
        return;
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        let owner = owner_fixture();
        register(&pg, &owner, "a").await?;
        register(&pg, &owner, "b").await?;

        pg.bind_inference_tier(&BindInferenceTierRequest {
            principal: owner.principal.clone(),
            org_id: Some(owner.org_id),
            tier: ModelTier::Fast,
            target_ref: "a".into(),
        })
        .await?;
        pg.bind_inference_tier(&BindInferenceTierRequest {
            principal: owner.principal.clone(),
            org_id: Some(owner.org_id),
            tier: ModelTier::Fast,
            target_ref: "b".into(),
        })
        .await?;

        let bindings = pg.list_inference_tier_bindings(&owner).await?;
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].target_ref, "b");
        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db).await;
    result.expect("bind inference tier upserts binding");
}
