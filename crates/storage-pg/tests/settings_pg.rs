//! End-to-end settings registration against a transient PG database.

mod common;

use common::{create_db, db_url, drop_db};
use proxima_core::models::{Dialect, EmbedCaps, LlmCaps, ModelTier};
use proxima_core::{OrgId, Owner, Principal, UserId};
use proxima_storage_pg::PgStorage;
use proxima_storage_pg::settings;
use uuid::Uuid;

fn fresh_owner() -> Owner {
    Owner {
        principal: Principal::User(UserId::new(Uuid::now_v7())),
        org_id: OrgId::new(Uuid::now_v7()),
    }
}

fn fresh_llm_model() -> settings::LlmModel {
    settings::LlmModel {
        vendor: "openai".to_string(),
        model_id: "gpt-4o".to_string(),
        dialect: Dialect::OpenAI,
        base_url: "https://api.openai.com/v1".to_string(),
        caps: LlmCaps {
            tool_use: true,
            json_mode: true,
            long_context: true,
            vision: true,
        },
        secret_ref: Some("openai-key".to_string()),
    }
}

fn fresh_embedding_model() -> settings::EmbeddingModel {
    settings::EmbeddingModel {
        vendor: "text-embeddings".to_string(),
        model_id: "text-embedding-3-small".to_string(),
        base_url: "https://api.openai.com/v1".to_string(),
        caps: EmbedCaps {
            dim: 1536,
            matryoshka: true,
        },
        secret_ref: Some("openai-key".to_string()),
    }
}

async fn with_db_test<F, Fut>(test_fn: F)
where
    F: FnOnce(PgStorage) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let db_name = format!("proxima_settings_test_{}", Uuid::now_v7().simple());
    if create_db(&db_name).await.is_err() {
        eprintln!("skipping (no admin PG)");
        return;
    }
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        test_fn(pg).await;
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    match result {
        Ok(()) => {}
        Err(e) => panic!("test failed: {e}"),
    }
}

// ============================================================================
// LLM model tests
// ============================================================================

#[tokio::test]
async fn register_llm_model_inserts() {
    with_db_test(|pg| async move {
        let owner = fresh_owner();
        let model = fresh_llm_model();

        pg.register_llm_model(&owner, model.clone())
            .await
            .expect("register succeeds");

        let models = pg.list_llm_models(&owner).await.expect("list succeeds");
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].vendor, model.vendor);
        assert_eq!(models[0].model_id, model.model_id);
        assert_eq!(models[0].dialect, model.dialect);
        assert_eq!(models[0].base_url, model.base_url);
        assert!(models[0].caps.tool_use);
        assert!(models[0].caps.json_mode);
        assert!(models[0].caps.long_context);
        assert!(models[0].caps.vision);
        assert_eq!(models[0].secret_ref, model.secret_ref);
    })
    .await;
}

#[tokio::test]
async fn register_llm_model_rejects_duplicate() {
    with_db_test(|pg| async move {
        let owner = fresh_owner();
        let model = fresh_llm_model();

        pg.register_llm_model(&owner, model.clone())
            .await
            .expect("first register succeeds");

        let err = pg
            .register_llm_model(&owner, model)
            .await
            .expect_err("second register fails");

        match err {
            settings::SettingsError::DuplicateLlmModel { vendor, model_id } => {
                assert_eq!(vendor, "openai");
                assert_eq!(model_id, "gpt-4o");
            }
            other => panic!("expected DuplicateLlmModel, got {other:?}"),
        }
    })
    .await;
}

// ============================================================================
// Embedding model tests
// ============================================================================

#[tokio::test]
async fn register_embedding_model_inserts() {
    with_db_test(|pg| async move {
        let owner = fresh_owner();
        let model = fresh_embedding_model();

        pg.register_embedding_model(&owner, model.clone())
            .await
            .expect("register succeeds");

        let models = pg
            .list_embedding_models(&owner)
            .await
            .expect("list succeeds");
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].vendor, model.vendor);
        assert_eq!(models[0].model_id, model.model_id);
        assert_eq!(models[0].base_url, model.base_url);
        assert_eq!(models[0].caps.dim, model.caps.dim);
        assert!(models[0].caps.matryoshka);
        assert_eq!(models[0].secret_ref, model.secret_ref);
    })
    .await;
}

#[tokio::test]
async fn register_embedding_model_rejects_duplicate() {
    with_db_test(|pg| async move {
        let owner = fresh_owner();
        let model = fresh_embedding_model();

        pg.register_embedding_model(&owner, model.clone())
            .await
            .expect("first register succeeds");

        let err = pg
            .register_embedding_model(&owner, model)
            .await
            .expect_err("second register fails");

        match err {
            settings::SettingsError::DuplicateEmbeddingModel { vendor, model_id } => {
                assert_eq!(vendor, "text-embeddings");
                assert_eq!(model_id, "text-embedding-3-small");
            }
            other => panic!("expected DuplicateEmbeddingModel, got {other:?}"),
        }
    })
    .await;
}

// ============================================================================
// Tier binding tests
// ============================================================================

#[tokio::test]
async fn bind_tier_happy_path() {
    with_db_test(|pg| async move {
        let owner = fresh_owner();
        let model = fresh_llm_model();

        pg.register_llm_model(&owner, model.clone())
            .await
            .expect("register model succeeds");

        pg.bind_tier(&owner, ModelTier::Fast, "openai", "gpt-4o")
            .await
            .expect("bind succeeds");

        let bindings = pg.list_tier_bindings(&owner).await.expect("list succeeds");
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].0, ModelTier::Fast);
        assert_eq!(bindings[0].1, "openai");
        assert_eq!(bindings[0].2, "gpt-4o");
    })
    .await;
}

#[tokio::test]
async fn bind_tier_rejects_unknown_model() {
    with_db_test(|pg| async move {
        let owner = fresh_owner();

        let err = pg
            .bind_tier(&owner, ModelTier::Fast, "unknown", "model")
            .await
            .expect_err("bind unknown model fails");

        match err {
            settings::SettingsError::UnknownLlmModel { vendor, model_id } => {
                assert_eq!(vendor, "unknown");
                assert_eq!(model_id, "model");
            }
            other => panic!("expected UnknownLlmModel, got {other:?}"),
        }
    })
    .await;
}

#[tokio::test]
async fn bind_tier_idempotent_rebind() {
    with_db_test(|pg| async move {
        let owner = fresh_owner();

        // Register two models
        pg.register_llm_model(
            &owner,
            settings::LlmModel {
                vendor: "openai".to_string(),
                model_id: "gpt-4o".to_string(),
                dialect: Dialect::OpenAI,
                base_url: "https://api.openai.com/v1".to_string(),
                caps: LlmCaps::none(),
                secret_ref: None,
            },
        )
        .await
        .expect("register first model");

        pg.register_llm_model(
            &owner,
            settings::LlmModel {
                vendor: "anthropic".to_string(),
                model_id: "claude-3-sonnet".to_string(),
                dialect: Dialect::Anthropic,
                base_url: "https://api.anthropic.com/v1".to_string(),
                caps: LlmCaps::none(),
                secret_ref: None,
            },
        )
        .await
        .expect("register second model");

        // Bind Fast to openai/gpt-4o
        pg.bind_tier(&owner, ModelTier::Fast, "openai", "gpt-4o")
            .await
            .expect("first bind");

        // Rebind Fast to anthropic/claude-3-sonnet
        pg.bind_tier(&owner, ModelTier::Fast, "anthropic", "claude-3-sonnet")
            .await
            .expect("rebind");

        let bindings = pg.list_tier_bindings(&owner).await.expect("list succeeds");
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].0, ModelTier::Fast);
        assert_eq!(bindings[0].1, "anthropic");
        assert_eq!(bindings[0].2, "claude-3-sonnet");
    })
    .await;
}

// ============================================================================
// Unbind tier tests
// ============================================================================

#[tokio::test]
async fn unbind_tier_returns_true_when_present() {
    with_db_test(|pg| async move {
        let owner = fresh_owner();

        pg.register_llm_model(&owner, fresh_llm_model())
            .await
            .expect("register model");
        pg.bind_tier(&owner, ModelTier::Fast, "openai", "gpt-4o")
            .await
            .expect("bind");

        let existed = pg
            .unbind_tier(&owner, ModelTier::Fast)
            .await
            .expect("unbind succeeds");
        assert!(existed);

        let bindings = pg.list_tier_bindings(&owner).await.expect("list succeeds");
        assert!(bindings.is_empty());
    })
    .await;
}

#[tokio::test]
async fn unbind_tier_returns_false_when_absent() {
    with_db_test(|pg| async move {
        let owner = fresh_owner();

        let existed = pg
            .unbind_tier(&owner, ModelTier::Fast)
            .await
            .expect("unbind succeeds");
        assert!(!existed);
    })
    .await;
}

// ============================================================================
// Embedding active tests
// ============================================================================

#[tokio::test]
async fn set_embedding_active_happy_path() {
    with_db_test(|pg| async move {
        let owner = fresh_owner();
        let model = fresh_embedding_model();

        pg.register_embedding_model(&owner, model.clone())
            .await
            .expect("register model");

        pg.set_embedding_active(&owner, "text-embeddings", "text-embedding-3-small")
            .await
            .expect("set active succeeds");

        let active = pg
            .get_embedding_active(&owner)
            .await
            .expect("get active succeeds");
        assert_eq!(
            active,
            Some((
                "text-embeddings".to_string(),
                "text-embedding-3-small".to_string()
            ))
        );
    })
    .await;
}

#[tokio::test]
async fn set_embedding_active_rejects_unknown() {
    with_db_test(|pg| async move {
        let owner = fresh_owner();

        let err = pg
            .set_embedding_active(&owner, "unknown", "model")
            .await
            .expect_err("set unknown active fails");

        match err {
            settings::SettingsError::UnknownEmbeddingModel { vendor, model_id } => {
                assert_eq!(vendor, "unknown");
                assert_eq!(model_id, "model");
            }
            other => panic!("expected UnknownEmbeddingModel, got {other:?}"),
        }
    })
    .await;
}

#[tokio::test]
async fn set_embedding_active_idempotent_rebind() {
    with_db_test(|pg| async move {
        let owner = fresh_owner();

        // Register two embedding models
        pg.register_embedding_model(
            &owner,
            settings::EmbeddingModel {
                vendor: "text-embeddings".to_string(),
                model_id: "text-embedding-3-small".to_string(),
                base_url: "https://api.openai.com/v1".to_string(),
                caps: EmbedCaps {
                    dim: 1536,
                    matryoshka: false,
                },
                secret_ref: None,
            },
        )
        .await
        .expect("register first");

        pg.register_embedding_model(
            &owner,
            settings::EmbeddingModel {
                vendor: "text-embeddings".to_string(),
                model_id: "text-embedding-3-large".to_string(),
                base_url: "https://api.openai.com/v1".to_string(),
                caps: EmbedCaps {
                    dim: 3072,
                    matryoshka: false,
                },
                secret_ref: None,
            },
        )
        .await
        .expect("register second");

        // Set active to first
        pg.set_embedding_active(&owner, "text-embeddings", "text-embedding-3-small")
            .await
            .expect("set first active");

        // Rebind to second
        pg.set_embedding_active(&owner, "text-embeddings", "text-embedding-3-large")
            .await
            .expect("rebind active");

        let active = pg.get_embedding_active(&owner).await.expect("get active");
        assert_eq!(
            active,
            Some((
                "text-embeddings".to_string(),
                "text-embedding-3-large".to_string()
            ))
        );
    })
    .await;
}

#[tokio::test]
async fn clear_embedding_active() {
    with_db_test(|pg| async move {
        let owner = fresh_owner();
        let model = fresh_embedding_model();

        pg.register_embedding_model(&owner, model)
            .await
            .expect("register model");

        pg.set_embedding_active(&owner, "text-embeddings", "text-embedding-3-small")
            .await
            .expect("set active");

        let existed = pg
            .clear_embedding_active(&owner)
            .await
            .expect("clear succeeds");
        assert!(existed);

        let active = pg.get_embedding_active(&owner).await.expect("get active");
        assert_eq!(active, None);

        // Clear again returns false
        let existed_again = pg
            .clear_embedding_active(&owner)
            .await
            .expect("clear again succeeds");
        assert!(!existed_again);
    })
    .await;
}

// ============================================================================
// Delete tests
// ============================================================================

#[tokio::test]
async fn delete_llm_model_happy_path() {
    with_db_test(|pg| async move {
        let owner = fresh_owner();
        let model = fresh_llm_model();

        pg.register_llm_model(&owner, model.clone())
            .await
            .expect("register succeeds");

        let existed = pg
            .delete_llm_model(&owner, "openai", "gpt-4o")
            .await
            .expect("delete succeeds");
        assert!(existed);

        let models = pg.list_llm_models(&owner).await.expect("list succeeds");
        assert!(models.is_empty());
    })
    .await;
}

#[tokio::test]
async fn delete_llm_model_returns_false_when_absent() {
    with_db_test(|pg| async move {
        let owner = fresh_owner();

        let existed = pg
            .delete_llm_model(&owner, "nonexistent", "model")
            .await
            .expect("delete succeeds");
        assert!(!existed);
    })
    .await;
}

#[tokio::test]
async fn delete_embedding_model_happy_path() {
    with_db_test(|pg| async move {
        let owner = fresh_owner();
        let model = fresh_embedding_model();

        pg.register_embedding_model(&owner, model.clone())
            .await
            .expect("register succeeds");

        let existed = pg
            .delete_embedding_model(&owner, "text-embeddings", "text-embedding-3-small")
            .await
            .expect("delete succeeds");
        assert!(existed);

        let models = pg
            .list_embedding_models(&owner)
            .await
            .expect("list succeeds");
        assert!(models.is_empty());
    })
    .await;
}

#[tokio::test]
async fn delete_embedding_model_returns_false_when_absent() {
    with_db_test(|pg| async move {
        let owner = fresh_owner();

        let existed = pg
            .delete_embedding_model(&owner, "nonexistent", "model")
            .await
            .expect("delete succeeds");
        assert!(!existed);
    })
    .await;
}

// ============================================================================
// Cross-owner isolation
// ============================================================================

#[tokio::test]
async fn cross_owner_isolation() {
    with_db_test(|pg| async move {
        let owner_a = fresh_owner();
        let owner_b = fresh_owner();

        pg.register_llm_model(&owner_a, fresh_llm_model())
            .await
            .expect("register for A");

        let models_a = pg.list_llm_models(&owner_a).await.expect("list A");
        assert_eq!(models_a.len(), 1);

        let models_b = pg.list_llm_models(&owner_b).await.expect("list B");
        assert!(models_b.is_empty());
    })
    .await;
}

// ============================================================================
// Caps roundtrip
// ============================================================================

#[tokio::test]
async fn caps_roundtrip_llm() {
    with_db_test(|pg| async move {
        let owner = fresh_owner();
        let model = settings::LlmModel {
            vendor: "anthropic".to_string(),
            model_id: "claude-3-opus".to_string(),
            dialect: Dialect::Anthropic,
            base_url: "https://api.anthropic.com/v1".to_string(),
            caps: LlmCaps {
                tool_use: true,
                json_mode: false,
                long_context: true,
                vision: false,
            },
            secret_ref: None,
        };

        pg.register_llm_model(&owner, model.clone())
            .await
            .expect("register");

        let models = pg.list_llm_models(&owner).await.expect("list");
        assert_eq!(models.len(), 1);
        assert!(models[0].caps.tool_use);
        assert!(!models[0].caps.json_mode);
        assert!(models[0].caps.long_context);
        assert!(!models[0].caps.vision);
    })
    .await;
}

#[tokio::test]
async fn caps_roundtrip_embedding() {
    with_db_test(|pg| async move {
        let owner = fresh_owner();
        let model = settings::EmbeddingModel {
            vendor: "text-embeddings".to_string(),
            model_id: "text-embedding-3-large".to_string(),
            base_url: "https://api.openai.com/v1".to_string(),
            caps: EmbedCaps {
                dim: 3072,
                matryoshka: true,
            },
            secret_ref: None,
        };

        pg.register_embedding_model(&owner, model.clone())
            .await
            .expect("register");

        let models = pg.list_embedding_models(&owner).await.expect("list");
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].caps.dim, 3072);
        assert!(models[0].caps.matryoshka);
    })
    .await;
}
