//! OAuth refresh client against auth.openai.com.

pub(crate) const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub(crate) const DEFAULT_TOKEN_ENDPOINT: &str = "https://auth.openai.com/oauth/token";

#[derive(Debug, Clone)]
pub struct RefreshedTokens {
    pub id_token: String,
    pub access_token: String,
    pub refresh_token: String,
}

#[derive(Debug)]
pub struct RefreshClient {
    http: reqwest::Client,
    endpoint: String,
}

#[derive(serde::Deserialize)]
struct RefreshResp {
    id_token: Option<String>,
    access_token: Option<String>,
    refresh_token: Option<String>,
}

impl RefreshClient {
    /// Production-default client targeting OpenAI's auth issuer.
    pub fn new(http: reqwest::Client) -> Self {
        Self {
            http,
            endpoint: DEFAULT_TOKEN_ENDPOINT.to_string(),
        }
    }

    /// Test/override variant: point at a different token endpoint (wiremock).
    pub fn with_endpoint(http: reqwest::Client, endpoint: impl Into<String>) -> Self {
        Self {
            http,
            endpoint: endpoint.into(),
        }
    }

    pub async fn refresh(
        &self,
        refresh_token: &str,
    ) -> Result<RefreshedTokens, crate::CodexAuthError> {
        let body = serde_json::json!({
            "client_id": CODEX_CLIENT_ID,
            "grant_type": "refresh_token",
            "refresh_token": refresh_token,
        });

        let response = self
            .http
            .post(&self.endpoint)
            .json(&body)
            .send()
            .await
            .map_err(|e| crate::CodexAuthError::Network(format!("refresh: {e}")))?;

        let status = response.status();
        if !status.is_success() {
            let body_text = response.text().await.unwrap_or_default();
            tracing::warn!(
                status = status.as_u16(),
                body = %body_text,
                "token refresh failed with non-2xx response"
            );
            return Err(crate::CodexAuthError::RefreshFailed);
        }

        let resp: RefreshResp = response
            .json()
            .await
            .map_err(|_| crate::CodexAuthError::RefreshFailed)?;

        match (resp.id_token, resp.access_token, resp.refresh_token) {
            (Some(id_token), Some(access_token), Some(refresh_token)) => {
                Ok(RefreshedTokens {
                    id_token,
                    access_token,
                    refresh_token,
                })
            }
            _ => Err(crate::CodexAuthError::RefreshFailed),
        }
    }
}
