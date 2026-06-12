use std::sync::Arc;
use std::time::{Duration, Instant};

use proxima_codex_auth::AuthDotJsonPath;
use proxima_core::error::ProtocolError;
use proxima_core::{AuthzContext, ChatGPTCodexConfig, Engine, InferenceTargetConfig, Owner};
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderName, HeaderValue};
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
pub struct CodexAuthStatusOutcomeTs {
    /// True if ~/.codex/auth.json exists and is readable.
    pub auth_json_present: bool,
    /// True if `tokens.access_token` is set and parseable as a JWT.
    pub access_token_present: bool,
}

#[tauri::command]
#[specta::specta]
pub async fn codex_auth_status() -> Result<CodexAuthStatusOutcomeTs, ProtocolError> {
    let Some(home) = std::env::var_os("HOME") else {
        return Ok(CodexAuthStatusOutcomeTs {
            auth_json_present: false,
            access_token_present: false,
        });
    };
    let path = std::path::PathBuf::from(home).join(".codex/auth.json");
    let Ok(bytes) = std::fs::read(&path) else {
        return Ok(CodexAuthStatusOutcomeTs {
            auth_json_present: false,
            access_token_present: false,
        });
    };
    let json: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => {
            return Ok(CodexAuthStatusOutcomeTs {
                auth_json_present: true,
                access_token_present: false,
            });
        }
    };
    let access_token_present = json["tokens"]["access_token"]
        .as_str()
        .is_some_and(|s| !s.is_empty());
    Ok(CodexAuthStatusOutcomeTs {
        auth_json_present: true,
        access_token_present,
    })
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
    fn not_supported(detail: impl Into<String>) -> Self {
        Self {
            code: "not_supported".into(),
            message: detail.into(),
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
    fn codex_auth_missing(message: String) -> Self {
        Self {
            code: "codex_auth_missing".into(),
            message,
        }
    }
    fn codex_auth_invalid(message: String) -> Self {
        Self {
            code: "codex_auth_invalid".into(),
            message,
        }
    }
    fn codex_auth_refresh_failed(message: String) -> Self {
        Self {
            code: "codex_auth_refresh_failed".into(),
            message,
        }
    }
}

fn config_api_key_env(config: &InferenceTargetConfig) -> Option<&str> {
    match config {
        InferenceTargetConfig::MistralChat(c) => Some(&c.api_key_env),
        InferenceTargetConfig::OpenAIChat(c) => Some(&c.api_key_env),
        InferenceTargetConfig::OpenAIResponses(c) => Some(&c.api_key_env),
        InferenceTargetConfig::ChatGPTCodex(_) => None,
    }
}

fn config_base_url(config: &InferenceTargetConfig) -> &str {
    match config {
        InferenceTargetConfig::MistralChat(c) => &c.base_url,
        InferenceTargetConfig::OpenAIChat(c) => &c.base_url,
        InferenceTargetConfig::OpenAIResponses(c) => &c.base_url,
        InferenceTargetConfig::ChatGPTCodex(c) => &c.base_url,
    }
}

fn config_model_id(config: &InferenceTargetConfig) -> &str {
    match config {
        InferenceTargetConfig::MistralChat(c) => &c.model_id,
        InferenceTargetConfig::OpenAIChat(c) => &c.model_id,
        InferenceTargetConfig::OpenAIResponses(c) => &c.model_id,
        InferenceTargetConfig::ChatGPTCodex(c) => &c.model_id,
    }
}

fn ping_endpoint(config: &InferenceTargetConfig) -> String {
    let base = config_base_url(config).trim_end_matches('/');
    match config {
        InferenceTargetConfig::MistralChat(_) | InferenceTargetConfig::OpenAIChat(_) => {
            format!("{base}/v1/chat/completions")
        }
        InferenceTargetConfig::OpenAIResponses(_) => {
            format!("{base}/responses")
        }
        InferenceTargetConfig::ChatGPTCodex(_) => {
            unreachable!("ChatGPTCodex is routed through ping_chatgpt_codex")
        }
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
        InferenceTargetConfig::ChatGPTCodex(_) => {
            unreachable!("ChatGPTCodex is routed through ping_chatgpt_codex")
        }
    }
}

fn codex_auth_to_ping_error(e: proxima_codex_auth::CodexAuthError) -> PingError {
    use proxima_codex_auth::CodexAuthError;
    match e {
        CodexAuthError::AuthJsonMissing { .. } => PingError::codex_auth_missing(e.to_string()),
        CodexAuthError::AuthJsonInvalid(_) | CodexAuthError::MissingAccountId => {
            PingError::codex_auth_invalid(e.to_string())
        }
        CodexAuthError::RefreshFailed => PingError::codex_auth_refresh_failed(e.to_string()),
        CodexAuthError::Network(detail) => PingError::network(detail),
    }
}

fn elapsed_ms(started: Instant) -> u32 {
    u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX)
}

/// Send one POST to `{base_url}/responses`. Returns `(status, latency_ms, body_text)`.
async fn chatgpt_codex_request(
    config: &ChatGPTCodexConfig,
    access_token: &str,
    account_id: &str,
) -> Result<(u16, u32, String), PingError> {
    let mut headers = HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {access_token}"))
            .map_err(|e| PingError::network(format!("invalid access_token header: {e}")))?,
    );
    headers.insert(
        HeaderName::from_static("chatgpt-account-id"),
        HeaderValue::from_str(account_id)
            .map_err(|e| PingError::network(format!("invalid account_id header: {e}")))?,
    );
    headers.insert(
        HeaderName::from_static("originator"),
        HeaderValue::from_static("proxima"),
    );

    let base = config.base_url.trim_end_matches('/');
    let url = format!("{base}/responses");
    // chatgpt.com/backend-api/codex/responses rejects requests without an
    // `instructions` field and expects `input` as an array of role+content
    // items (not a bare string). Mirrors the shape Goose's chatgpt_codex
    // provider builds — minimal version for a connectivity ping.
    let mut body = json!({
        "model": config.model_id,
        "instructions": "Reply with the single word: pong.",
        "input": [{
            "role": "user",
            "content": [{ "type": "input_text", "text": "ping" }],
        }],
        "store": false,
        "stream": true,
    });
    if let Some(effort) = config.reasoning_effort.as_deref()
        && let Some(obj) = body.as_object_mut()
    {
        obj.insert("reasoning".to_string(), json!({ "effort": effort }));
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| PingError::network(format!("client build: {e}")))?;

    let started = Instant::now();
    let resp = client
        .post(&url)
        .headers(headers)
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

    let latency_ms = elapsed_ms(started);
    let status = resp.status().as_u16();
    let body_text = resp.text().await.unwrap_or_default();
    Ok((status, latency_ms, body_text))
}

/// Inner implementation that accepts the auth.json path explicitly —
/// allows tests to inject a fixture path without mutating process env
/// (which would require `unsafe` in edition 2024).
async fn ping_chatgpt_codex_with(
    config: &ChatGPTCodexConfig,
    auth_json: AuthDotJsonPath,
) -> Result<u32, PingError> {
    use proxima_codex_auth::CodexAuthResolver;

    let resolver = CodexAuthResolver::new(auth_json)
        .map_err(|e| PingError::network(format!("codex auth resolver: {e}")))?;

    // First attempt with the proactively-resolved credentials.
    let creds = resolver.resolve().await.map_err(codex_auth_to_ping_error)?;
    let (status, latency_ms, body_text) =
        chatgpt_codex_request(config, &creds.access_token, &creds.account_id).await?;
    if (200..300).contains(&status) {
        return Ok(latency_ms);
    }

    // On 401, force a refresh and retry exactly once.
    if status == 401 {
        let refreshed = resolver
            .invalidate_and_refresh()
            .await
            .map_err(codex_auth_to_ping_error)?;
        let (status2, latency_ms2, body_text2) =
            chatgpt_codex_request(config, &refreshed.access_token, &refreshed.account_id).await?;
        if (200..300).contains(&status2) {
            return Ok(latency_ms2);
        }
        let excerpt: String = body_text2.chars().take(200).collect();
        return Err(PingError::http(status2, excerpt));
    }

    let excerpt: String = body_text.chars().take(200).collect();
    Err(PingError::http(status, excerpt))
}

async fn ping_chatgpt_codex(config: &ChatGPTCodexConfig) -> Result<u32, PingError> {
    let home = std::env::var_os("HOME")
        .ok_or_else(|| PingError::network("HOME env var not set".to_string()))?;
    let auth_json = AuthDotJsonPath::from_home(&std::path::PathBuf::from(home));
    ping_chatgpt_codex_with(config, auth_json).await
}

async fn ping_via_env_key<F>(
    config: &InferenceTargetConfig,
    env_lookup: F,
) -> Result<u32, PingError>
where
    F: Fn(&str) -> Option<String>,
{
    let env_var = config_api_key_env(config).ok_or_else(|| {
        PingError::not_supported("no env-var-based credential resolver for this target kind")
    })?;
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
    let latency_ms = elapsed_ms(started);
    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body_text = resp.text().await.unwrap_or_default();
        let excerpt: String = body_text.chars().take(200).collect();
        return Err(PingError::http(status, excerpt));
    }
    Ok(latency_ms)
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
    match config {
        InferenceTargetConfig::ChatGPTCodex(c) => ping_chatgpt_codex(c).await,
        _ => ping_via_env_key(config, env_lookup).await,
    }
}

pub async fn ping_target(config: &InferenceTargetConfig) -> Result<u32, PingError> {
    ping_target_with(config, |key| std::env::var(key).ok()).await
}

#[tauri::command]
#[specta::specta]
pub async fn test_inference_target(
    engine: State<'_, Arc<Engine>>,
    authz: State<'_, AuthzContext>,
    req: TestInferenceTargetTs,
) -> Result<TestInferenceTargetOutcomeTs, ProtocolError> {
    let rows = engine.list_inference_targets(&authz, &req.owner).await?;
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
            latency_ms: elapsed_ms(started),
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
            reasoning_effort: None,

            context_window_tokens: None,
        });
        let result = ping_target_with(&config, |_| None).await;
        let err = result.expect_err("must error when env unset");
        assert_eq!(err.code(), "env_missing");
        assert!(err.message().contains("PROXIMA_TEST_PING_NO_KEY"));
    }

    fn make_fresh_access_token(account_id: &str) -> String {
        use base64::Engine as _;
        let exp = i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        )
        .unwrap_or(i64::MAX)
            + 3600;
        let claims = serde_json::json!({
            "chatgpt_account_id": account_id,
            "exp": exp,
        });
        let payload_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&claims).unwrap());
        format!("header.{payload_b64}.signature")
    }

    #[tokio::test]
    async fn ping_chatgpt_codex_uses_codex_oauth_against_configured_endpoint() {
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        // Stand up wiremock as the "chatgpt.com/backend-api/codex" server.
        let server = MockServer::start().await;

        let fresh_access = make_fresh_access_token("acct-test");

        // Expect POST {server}/responses with the three required headers and correct body.
        Mock::given(method("POST"))
            .and(path("/responses"))
            .and(header(
                "authorization",
                format!("Bearer {fresh_access}").as_str(),
            ))
            .and(header("chatgpt-account-id", "acct-test"))
            .and(header("originator", "proxima"))
            .and(wiremock::matchers::body_json(serde_json::json!({
                "model": "gpt-5.3-codex",
                "instructions": "Reply with the single word: pong.",
                "input": [{
                    "role": "user",
                    "content": [{ "type": "input_text", "text": "ping" }],
                }],
                "store": false,
                "stream": true,
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "resp_001",
                "model": "gpt-5.3-codex",
                "output": []
            })))
            .mount(&server)
            .await;

        // Build a temp HOME with a fixture auth.json carrying fresh tokens.
        let tmp = tempfile::tempdir().unwrap();
        let codex_dir = tmp.path().join(".codex");
        std::fs::create_dir(&codex_dir).unwrap();
        let auth_json_value = serde_json::json!({
            "tokens": {
                "id_token": "id",
                "access_token": fresh_access,
                "refresh_token": "ref",
            }
        });
        std::fs::write(codex_dir.join("auth.json"), auth_json_value.to_string()).unwrap();

        let config = ChatGPTCodexConfig {
            base_url: server.uri(),
            model_id: "gpt-5.3-codex".to_string(),
            reasoning_effort: None,

            context_window_tokens: None,
        };
        let auth = AuthDotJsonPath(codex_dir.join("auth.json"));

        let latency_ms = ping_chatgpt_codex_with(&config, auth)
            .await
            .expect("ping should succeed");
        assert!(
            latency_ms < 5_000,
            "ping latency reasonable: {latency_ms}ms"
        );
    }
}
