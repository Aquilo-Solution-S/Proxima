//! Codex (~/.codex/auth.json) credential resolver for Proxima.

mod auth_json;
mod jwt;
mod refresh;

pub use auth_json::AuthDotJsonPath;

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
    #[expect(dead_code, reason = "used in resolve() which is stubbed for later tasks")]
    auth_json: AuthDotJsonPath,
    #[expect(dead_code, reason = "used in resolve() which is stubbed for later tasks")]
    http: reqwest::Client,
}

impl CodexAuthResolver {
    pub fn new(auth_json: AuthDotJsonPath) -> Result<Self, CodexAuthError> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .map_err(|e| CodexAuthError::Network(format!("reqwest builder: {e}")))?;
        Ok(Self { auth_json, http })
    }

    pub async fn resolve(&self) -> Result<CodexCredentials, CodexAuthError> {
        unimplemented!("filled in subsequent tasks")
    }
}
