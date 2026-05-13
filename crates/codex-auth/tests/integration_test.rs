//! Integration tests for the codex-auth refresh client.

use proxima_codex_auth::{CodexAuthError, RefreshClient};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn refresh_success_returns_triple() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id_token": "new-id",
            "access_token": "new-access",
            "refresh_token": "new-refresh",
        })))
        .mount(&server)
        .await;
    let http = reqwest::Client::new();
    let client = RefreshClient::with_endpoint(http, format!("{}/oauth/token", server.uri()));
    let tokens = client.refresh("old-refresh").await.unwrap();
    assert_eq!(tokens.id_token, "new-id");
    assert_eq!(tokens.access_token, "new-access");
    assert_eq!(tokens.refresh_token, "new-refresh");
}

#[tokio::test]
async fn refresh_401_returns_refresh_failed() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(
            ResponseTemplate::new(401).set_body_string(r#"{"error":"unauthorized"}"#),
        )
        .mount(&server)
        .await;
    let http = reqwest::Client::new();
    let client = RefreshClient::with_endpoint(http, format!("{}/oauth/token", server.uri()));
    let err = client.refresh("dead").await.unwrap_err();
    assert!(matches!(err, CodexAuthError::RefreshFailed), "got {err:?}");
}

#[tokio::test]
async fn refresh_500_returns_refresh_failed() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let http = reqwest::Client::new();
    let client = RefreshClient::with_endpoint(http, format!("{}/oauth/token", server.uri()));
    let err = client.refresh("anything").await.unwrap_err();
    assert!(matches!(err, CodexAuthError::RefreshFailed), "got {err:?}");
}

#[tokio::test]
async fn refresh_missing_field_returns_refresh_failed() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id_token": "new-id",
            "access_token": "new-access",
            // refresh_token intentionally missing
        })))
        .mount(&server)
        .await;
    let http = reqwest::Client::new();
    let client = RefreshClient::with_endpoint(http, format!("{}/oauth/token", server.uri()));
    let err = client.refresh("anything").await.unwrap_err();
    assert!(matches!(err, CodexAuthError::RefreshFailed), "got {err:?}");
}

#[tokio::test]
async fn refresh_sends_correct_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(wiremock::matchers::body_json(serde_json::json!({
            "client_id": "app_EMoamEEZ73f0CkXaXp7hrann",
            "grant_type": "refresh_token",
            "refresh_token": "the-old-refresh",
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id_token": "x", "access_token": "y", "refresh_token": "z",
        })))
        .mount(&server)
        .await;
    let http = reqwest::Client::new();
    let client = RefreshClient::with_endpoint(http, format!("{}/oauth/token", server.uri()));
    let _ = client.refresh("the-old-refresh").await.unwrap();
    // If body didn't match, the mock would not have responded with 200 and unwrap would panic.
}
