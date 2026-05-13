//! Integration tests for the codex-auth refresh client.

use proxima_codex_auth::{AuthDotJsonPath, CodexAuthError, CodexAuthResolver, RefreshClient};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// ---------------------------------------------------------------------------
// Token fixture helpers
// ---------------------------------------------------------------------------

fn fresh_exp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
        + 3600 // 1 hour in the future
}

fn stale_exp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
        - 60 // 60s in the past
}

fn jwt_with_claims(claims: serde_json::Value) -> String {
    use base64::Engine as _;
    let payload_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(serde_json::to_vec(&claims).unwrap());
    format!("header.{payload_b64}.signature")
}

fn write_auth_json(dir: &std::path::Path, body: serde_json::Value) -> PathBuf {
    let path = dir.join("auth.json");
    std::fs::write(&path, body.to_string()).unwrap();
    path
}

// ---------------------------------------------------------------------------
// Existing refresh-client tests (Tasks 1-4)
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Task 5: resolver integration tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn resolve_returns_credentials_when_tokens_fresh() {
    let dir = tempfile::tempdir().unwrap();
    let access = jwt_with_claims(serde_json::json!({
        "chatgpt_account_id": "acct-fresh",
        "exp": fresh_exp(),
    }));
    let path = write_auth_json(
        dir.path(),
        serde_json::json!({
            "tokens": {
                "id_token": "id",
                "access_token": access,
                "refresh_token": "ref",
            }
        }),
    );

    // Wiremock that would FAIL the test if hit — fresh token must NOT trigger refresh.
    let server = MockServer::start().await;
    let http = reqwest::Client::new();
    let refresh = RefreshClient::with_endpoint(http, format!("{}/oauth/token", server.uri()));
    let resolver = CodexAuthResolver::with_refresh_client(AuthDotJsonPath(path.clone()), refresh);

    let creds = resolver.resolve().await.unwrap();
    assert_eq!(creds.account_id, "acct-fresh");
    assert!(!creds.access_token.is_empty());
    // No mocks defined → wiremock would 404 on any call → if resolver hit it, RefreshFailed would surface.
}

#[tokio::test]
async fn resolve_refreshes_and_persists_when_exp_within_skew() {
    let dir = tempfile::tempdir().unwrap();
    let stale_access = jwt_with_claims(serde_json::json!({
        "chatgpt_account_id": "acct-stale",
        "exp": stale_exp(),
    }));
    let new_access = jwt_with_claims(serde_json::json!({
        "chatgpt_account_id": "acct-after-refresh",
        "exp": fresh_exp(),
    }));
    let auth_path = write_auth_json(
        dir.path(),
        serde_json::json!({
            "tokens": {
                "id_token": "id-old",
                "access_token": stale_access,
                "refresh_token": "ref-old",
            }
        }),
    );

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id_token": "id-new",
                "access_token": new_access.clone(),
                "refresh_token": "ref-new",
            })),
        )
        .mount(&server)
        .await;

    let http = reqwest::Client::new();
    let refresh = RefreshClient::with_endpoint(http, format!("{}/oauth/token", server.uri()));
    let resolver =
        CodexAuthResolver::with_refresh_client(AuthDotJsonPath(auth_path.clone()), refresh);

    let creds = resolver.resolve().await.unwrap();
    assert_eq!(creds.account_id, "acct-after-refresh");
    assert_eq!(creds.access_token, new_access);

    // Verify auth.json was rewritten with the new tokens.
    let persisted: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&auth_path).unwrap()).unwrap();
    assert_eq!(persisted["tokens"]["id_token"], "id-new");
    assert_eq!(persisted["tokens"]["access_token"], new_access);
    assert_eq!(persisted["tokens"]["refresh_token"], "ref-new");
}

#[tokio::test]
async fn resolve_falls_back_to_tokens_account_id_field_when_jwt_claim_absent() {
    let dir = tempfile::tempdir().unwrap();
    let access = jwt_with_claims(serde_json::json!({ "exp": fresh_exp() })); // no chatgpt_account_id
    let auth_path = write_auth_json(
        dir.path(),
        serde_json::json!({
            "tokens": {
                "id_token": "id",
                "access_token": access,
                "refresh_token": "ref",
                "account_id": "acct-from-field",
            }
        }),
    );

    let server = MockServer::start().await;
    let http = reqwest::Client::new();
    let refresh = RefreshClient::with_endpoint(http, format!("{}/oauth/token", server.uri()));
    let resolver = CodexAuthResolver::with_refresh_client(AuthDotJsonPath(auth_path), refresh);

    let creds = resolver.resolve().await.unwrap();
    assert_eq!(creds.account_id, "acct-from-field");
}

#[tokio::test]
async fn resolve_returns_missing_account_id_when_both_paths_absent() {
    let dir = tempfile::tempdir().unwrap();
    let access = jwt_with_claims(serde_json::json!({ "exp": fresh_exp() })); // no chatgpt_account_id
    let auth_path = write_auth_json(
        dir.path(),
        serde_json::json!({
            "tokens": {
                "id_token": "id",
                "access_token": access,
                "refresh_token": "ref",
                // No account_id field either
            }
        }),
    );

    let server = MockServer::start().await;
    let http = reqwest::Client::new();
    let refresh = RefreshClient::with_endpoint(http, format!("{}/oauth/token", server.uri()));
    let resolver = CodexAuthResolver::with_refresh_client(AuthDotJsonPath(auth_path), refresh);

    let err = resolver.resolve().await.unwrap_err();
    assert!(
        matches!(err, CodexAuthError::MissingAccountId),
        "got {err:?}"
    );
}

#[tokio::test]
async fn resolve_returns_refresh_failed_when_endpoint_401s() {
    let dir = tempfile::tempdir().unwrap();
    let stale_access = jwt_with_claims(serde_json::json!({
        "chatgpt_account_id": "acct-stale",
        "exp": stale_exp(),
    }));
    let auth_path = write_auth_json(
        dir.path(),
        serde_json::json!({
            "tokens": {
                "id_token": "id-old",
                "access_token": stale_access,
                "refresh_token": "ref-old",
            }
        }),
    );

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(401).set_body_string("{}"))
        .mount(&server)
        .await;

    let http = reqwest::Client::new();
    let refresh = RefreshClient::with_endpoint(http, format!("{}/oauth/token", server.uri()));
    let resolver = CodexAuthResolver::with_refresh_client(AuthDotJsonPath(auth_path), refresh);

    let err = resolver.resolve().await.unwrap_err();
    assert!(
        matches!(err, CodexAuthError::RefreshFailed),
        "got {err:?}"
    );
}

#[tokio::test]
async fn resolve_returns_invalid_when_tokens_missing_from_auth_json() {
    let dir = tempfile::tempdir().unwrap();
    let auth_path = write_auth_json(
        dir.path(),
        serde_json::json!({
            "OPENAI_API_KEY": null,
            // no "tokens" key at all
        }),
    );

    let server = MockServer::start().await;
    let http = reqwest::Client::new();
    let refresh = RefreshClient::with_endpoint(http, format!("{}/oauth/token", server.uri()));
    let resolver = CodexAuthResolver::with_refresh_client(AuthDotJsonPath(auth_path), refresh);

    let err = resolver.resolve().await.unwrap_err();
    assert!(
        matches!(err, CodexAuthError::AuthJsonInvalid(_)),
        "got {err:?}"
    );
}
