//! Codex (~/.codex/auth.json) credential resolver for Proxima.

mod auth_json;
mod jwt;
mod refresh;

pub use auth_json::AuthDotJsonPath;
pub use jwt::{decode_chatgpt_claims, ChatGptClaims};
pub use refresh::{RefreshClient, RefreshedTokens};

const REFRESH_SKEW_SECS: i64 = 30;

#[derive(Debug, Clone)]
pub struct CodexCredentials {
    pub access_token: String,
    pub account_id: String,
}

#[derive(Debug, thiserror::Error)]
pub enum CodexAuthError {
    #[error("auth.json not found at {path}; run `codex login`")]
    AuthJsonMissing { path: String },
    #[error("auth.json is unreadable or malformed: {0}")]
    AuthJsonInvalid(String),
    #[error("access_token has no chatgpt_account_id claim; re-run `codex login`")]
    MissingAccountId,
    #[error("token refresh failed; run `codex` once to re-authenticate")]
    RefreshFailed,
    #[error("network error: {0}")]
    Network(String),
}

#[derive(Debug)]
pub struct CodexAuthResolver {
    auth_json: AuthDotJsonPath,
    refresh_client: RefreshClient,
}

impl CodexAuthResolver {
    /// Production constructor: builds a reqwest client and the default-endpoint
    /// refresh client.
    pub fn new(auth_json: AuthDotJsonPath) -> Result<Self, CodexAuthError> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .map_err(|e| CodexAuthError::Network(format!("reqwest builder: {e}")))?;
        Ok(Self {
            auth_json,
            refresh_client: RefreshClient::new(http),
        })
    }

    /// Test / advanced constructor that injects a pre-built RefreshClient
    /// (e.g. one pointed at a wiremock endpoint).
    pub fn with_refresh_client(auth_json: AuthDotJsonPath, refresh_client: RefreshClient) -> Self {
        Self {
            auth_json,
            refresh_client,
        }
    }

    pub async fn resolve(&self) -> Result<CodexCredentials, CodexAuthError> {
        let mut value = self.auth_json.read()?;
        let mut access_token = read_token_field(&value, "access_token")?;
        let refresh_token = read_token_field(&value, "refresh_token")?;
        let mut claims = crate::jwt::decode_chatgpt_claims(&access_token)?;

        if needs_refresh(claims.exp) {
            let refreshed = self.refresh_client.refresh(&refresh_token).await?;
            apply_refresh(&mut value, &refreshed);
            if let Err(err) = self.auth_json.write_atomic(&value) {
                tracing::warn!(error = %err,
                    "codex_auth: refreshed tokens but could not persist back to auth.json");
            }
            access_token = refreshed.access_token;
            claims = crate::jwt::decode_chatgpt_claims(&access_token)?;
        }

        let account_id = claims
            .chatgpt_account_id
            .or_else(|| {
                value["tokens"]
                    .get("account_id")
                    .and_then(|v| v.as_str())
                    .map(String::from)
            })
            .ok_or(CodexAuthError::MissingAccountId)?;

        Ok(CodexCredentials {
            access_token,
            account_id,
        })
    }
}

fn read_token_field(value: &serde_json::Value, field: &str) -> Result<String, CodexAuthError> {
    value["tokens"][field]
        .as_str()
        .map(String::from)
        .ok_or_else(|| CodexAuthError::AuthJsonInvalid(format!("missing tokens.{field}")))
}

fn needs_refresh(exp: Option<i64>) -> bool {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    match exp {
        Some(exp) => now + REFRESH_SKEW_SECS >= exp,
        None => true,
    }
}

fn apply_refresh(value: &mut serde_json::Value, r: &crate::refresh::RefreshedTokens) {
    let tokens = match value.get_mut("tokens").and_then(|t| t.as_object_mut()) {
        Some(t) => t,
        None => return,
    };
    tokens.insert("id_token".to_string(), r.id_token.clone().into());
    tokens.insert("access_token".to_string(), r.access_token.clone().into());
    tokens.insert("refresh_token".to_string(), r.refresh_token.clone().into());
}
