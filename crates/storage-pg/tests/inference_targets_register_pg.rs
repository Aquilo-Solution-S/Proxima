//! Register / replace `InferenceTarget` rows.

mod common;

use common::{drop_db, fresh_pg, owner_fixture};
use proxima_core::{
    InferenceTargetConfig, LocalCliConfig, RegisterInferenceTargetRequest, Storage,
};

fn local_cli(command: &str, profile: Option<&str>) -> InferenceTargetConfig {
    InferenceTargetConfig::LocalCli(LocalCliConfig {
        command: command.into(),
        profile: profile.map(str::to_string),
        env_overrides: vec![],
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn register_inference_target_inserts_new_row() {
    let Some((pg, db)) = fresh_pg().await else {
        return;
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        let owner = owner_fixture();

        let req = RegisterInferenceTargetRequest {
            owner: owner.clone(),
            target_ref: "local-goose".into(),
            config: local_cli("goose", Some("default")),
        };
        let out = pg.register_inference_target(&req).await?;
        assert_eq!(out.target_ref, "local-goose");
        assert!(!out.idempotent_replay);

        let rows = pg.list_inference_targets(&owner).await?;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].target_ref, "local-goose");
        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db).await;
    result.expect("register inference target inserts row");
}

#[tokio::test(flavor = "multi_thread")]
async fn register_inference_target_idempotent_when_body_matches() {
    let Some((pg, db)) = fresh_pg().await else {
        return;
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        let owner = owner_fixture();
        let req = RegisterInferenceTargetRequest {
            owner,
            target_ref: "local-goose".into(),
            config: local_cli("goose", None),
        };
        let _ = pg.register_inference_target(&req).await?;
        let replay = pg.register_inference_target(&req).await?;
        assert!(replay.idempotent_replay);
        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db).await;
    result.expect("register inference target is idempotent");
}

#[tokio::test(flavor = "multi_thread")]
async fn register_inference_target_conflict_when_body_differs() {
    let Some((pg, db)) = fresh_pg().await else {
        return;
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        let owner = owner_fixture();
        let req_a = RegisterInferenceTargetRequest {
            owner: owner.clone(),
            target_ref: "local-goose".into(),
            config: local_cli("goose", None),
        };
        let _ = pg.register_inference_target(&req_a).await?;

        let req_b = RegisterInferenceTargetRequest {
            owner,
            target_ref: "local-goose".into(),
            config: local_cli("/usr/local/bin/goose", None),
        };
        let err = pg
            .register_inference_target(&req_b)
            .await
            .expect_err("should conflict");
        let msg = format!("{err}");
        assert!(msg.to_lowercase().contains("conflict"), "got {msg}");
        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db).await;
    result.expect("register inference target detects conflict");
}
