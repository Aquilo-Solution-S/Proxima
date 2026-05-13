use std::sync::Arc;
use std::time::{Duration, Instant};

use proxima_core::auth::Credentials;
use proxima_core::error::ProtocolError;
use proxima_core::{Engine, InferenceTargetConfig, Owner};
use serde_json::json;
use tauri::State;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct InferenceEnvStatusTs {
    pub env_var: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct InferenceEnvStatusOutcomeTs {
    pub present: bool,
}

fn env_status_with<F>(env_var: &str, lookup: F) -> InferenceEnvStatusOutcomeTs
where
    F: Fn(&str) -> bool,
{
    InferenceEnvStatusOutcomeTs {
        present: lookup(env_var),
    }
}

#[tauri::command]
#[specta::specta]
pub async fn inference_env_status(
    req: InferenceEnvStatusTs,
) -> Result<InferenceEnvStatusOutcomeTs, ProtocolError> {
    Ok(env_status_with(&req.env_var, |key| {
        std::env::var(key).is_ok()
    }))
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct TestInferenceTargetTs {
    pub owner: Owner,
    pub target_ref: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct TestInferenceTargetOutcomeTs {
    pub ok: bool,
    pub latency_ms: u32,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
}

#[derive(Debug)]
pub struct PingError {
    code: String,
    message: String,
}

impl PingError {
    pub fn code(&self) -> &str {
        &self.code
    }
    pub fn message(&self) -> &str {
        &self.message
    }
    fn env_missing(env_var: &str) -> Self {
        Self {
            code: "env_missing".into(),
            message: format!("env var {env_var} is not set"),
        }
    }
    fn network(detail: String) -> Self {
        Self {
            code: "network".into(),
            message: detail,
        }
    }
    fn http(status: u16, body_excerpt: String) -> Self {
        Self {
            code: format!("http_{status}"),
            message: body_excerpt,
        }
    }
    fn timeout() -> Self {
        Self {
            code: "timeout".into(),
            message: "request exceeded 5s".into(),
        }
    }
}

fn config_api_key_env(config: &InferenceTargetConfig) -> &str {
    match config {
        InferenceTargetConfig::MistralChat(c) => &c.api_key_env,
        InferenceTargetConfig::OpenAIChat(c) => &c.api_key_env,
        InferenceTargetConfig::OpenAIResponses(c) => &c.api_key_env,
    }
}

fn config_base_url(config: &InferenceTargetConfig) -> &str {
    match config {
        InferenceTargetConfig::MistralChat(c) => &c.base_url,
        InferenceTargetConfig::OpenAIChat(c) => &c.base_url,
        InferenceTargetConfig::OpenAIResponses(c) => &c.base_url,
    }
}

fn config_model_id(config: &InferenceTargetConfig) -> &str {
    match config {
        InferenceTargetConfig::MistralChat(c) => &c.model_id,
        InferenceTargetConfig::OpenAIChat(c) => &c.model_id,
        InferenceTargetConfig::OpenAIResponses(c) => &c.model_id,
    }
}

fn ping_endpoint(config: &InferenceTargetConfig) -> String {
    let base = config_base_url(config).trim_end_matches('/');
    match config {
        InferenceTargetConfig::MistralChat(_) | InferenceTargetConfig::OpenAIChat(_) => {
            format!("{base}/v1/chat/completions")
        }
        InferenceTargetConfig::OpenAIResponses(_) => format!("{base}/v1/responses"),
    }
}

fn ping_body(config: &InferenceTargetConfig) -> serde_json::Value {
    let model = config_model_id(config);
    match config {
        InferenceTargetConfig::MistralChat(_) | InferenceTargetConfig::OpenAIChat(_) => json!({
            "model": model,
            "messages": [{ "role": "user", "content": "ping" }],
            "max_tokens": 1,
        }),
        InferenceTargetConfig::OpenAIResponses(_) => json!({
            "model": model,
            "input": "ping",
            "max_output_tokens": 16,
        }),
    }
}

/// Inner function that accepts an env-lookup closure so tests can supply
/// a stub without mutating process env (the crate forbids `unsafe`).
pub async fn ping_target_with<F>(
    config: &InferenceTargetConfig,
    env_lookup: F,
) -> Result<u32, PingError>
where
    F: Fn(&str) -> Option<String>,
{
    let env_var = config_api_key_env(config);
    let api_key = env_lookup(env_var).ok_or_else(|| PingError::env_missing(env_var))?;
    let url = ping_endpoint(config);
    let body = ping_body(config);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| PingError::network(format!("client build: {e}")))?;
    let started = Instant::now();
    let resp = client
        .post(&url)
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .await
        .map_err(|e| {
            if e.is_timeout() {
                PingError::timeout()
            } else {
                PingError::network(e.to_string())
            }
        })?;
    let latency_ms = started.elapsed().as_millis() as u32;
    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body_text = resp.text().await.unwrap_or_default();
        let excerpt: String = body_text.chars().take(200).collect();
        return Err(PingError::http(status, excerpt));
    }
    Ok(latency_ms)
}

pub async fn ping_target(config: &InferenceTargetConfig) -> Result<u32, PingError> {
    ping_target_with(config, |key| std::env::var(key).ok()).await
}

#[tauri::command]
#[specta::specta]
pub async fn test_inference_target(
    engine: State<'_, Arc<Engine>>,
    req: TestInferenceTargetTs,
) -> Result<TestInferenceTargetOutcomeTs, ProtocolError> {
    let rows = engine
        .list_inference_targets(&Credentials::None, &req.owner)
        .await?;
    let row = rows
        .iter()
        .find(|row| row.target_ref == req.target_ref)
        .ok_or_else(|| {
            ProtocolError::invalid_argument(
                "target_ref",
                format!("no target registered with ref '{}'", req.target_ref),
            )
        })?;
    let started = Instant::now();
    match ping_target(&row.config).await {
        Ok(latency_ms) => Ok(TestInferenceTargetOutcomeTs {
            ok: true,
            latency_ms,
            error_code: None,
            error_message: None,
        }),
        Err(err) => Ok(TestInferenceTargetOutcomeTs {
            ok: false,
            latency_ms: started.elapsed().as_millis() as u32,
            error_code: Some(err.code().to_string()),
            error_message: Some(err.message().to_string()),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_status_present_when_lookup_returns_true() {
        let out = env_status_with("ANY_KEY", |_| true);
        assert!(out.present);
    }

    #[test]
    fn env_status_absent_when_lookup_returns_false() {
        let out = env_status_with("ANY_KEY", |_| false);
        assert!(!out.present);
    }

    #[tokio::test]
    async fn ping_returns_env_missing_when_lookup_returns_none() {
        let config = InferenceTargetConfig::MistralChat(proxima_core::MistralChatConfig {
            base_url: "https://api.mistral.ai".into(),
            model_id: "ignored".into(),
            api_key_env: "PROXIMA_TEST_PING_NO_KEY".into(),
            temperature: None,
            max_completion_tokens: None,
        });
        let result = ping_target_with(&config, |_| None).await;
        let err = result.expect_err("must error when env unset");
        assert_eq!(err.code(), "env_missing");
        assert!(err.message().contains("PROXIMA_TEST_PING_NO_KEY"));
    }
}
