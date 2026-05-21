mod common;

use common::{drop_db, fresh_pg, owner_fixture};
use proxima_core::{
    InferenceTargetConfig, MistralChatConfig, RegisterInferenceTargetRequest, Storage,
};

#[tokio::test(flavor = "multi_thread")]
async fn list_inference_targets_returns_owner_scoped_rows_in_order() {
    let Some((pg, db)) = fresh_pg().await else {
        return;
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        let owner = owner_fixture();

        for slug in ["zeta", "alpha", "kappa"] {
            let req = RegisterInferenceTargetRequest {
                owner: owner.clone(),
                target_ref: slug.into(),
                config: InferenceTargetConfig::MistralChat(MistralChatConfig {
                    base_url: "http://127.0.0.1:9".into(),
                    model_id: "test-model".into(),
                    api_key_env: "PATH".into(),
                    temperature: None,
                    max_completion_tokens: None,
                    reasoning_effort: None,
                }),
            };
            pg.register_inference_target(&req).await?;
        }

        let rows = pg.list_inference_targets(&owner).await?;
        let names: Vec<_> = rows.iter().map(|r| r.target_ref.as_str()).collect();
        assert_eq!(names, vec!["alpha", "kappa", "zeta"]);
        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db).await;
    result.expect("list inference targets returns ordered rows");
}
