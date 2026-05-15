use base64::Engine as _;

use crate::CodexAuthError;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChatGptClaims {
    pub chatgpt_account_id: Option<String>,
    pub exp: Option<i64>,
}

/// Decode `chatgpt_account_id` and `exp` from the payload of a signed JWT.
///
/// Signature validation is intentionally skipped — `OpenAI`'s server validates
/// the signature at the point of use. We only need the claims for routing
/// (`account_id`) and freshness checks (`exp`).
///
/// # Errors
///
/// Returns [`CodexAuthError::AuthJsonInvalid`] when the token is not a
/// three-segment JWT, the payload is not URL-safe base64, or the decoded
/// payload is not JSON.
pub fn decode_chatgpt_claims(token: &str) -> Result<ChatGptClaims, CodexAuthError> {
    // A JWT is exactly header.payload.signature — three dot-separated segments.
    let parts: Vec<&str> = token.splitn(4, '.').collect();
    if parts.len() != 3 {
        return Err(CodexAuthError::AuthJsonInvalid(
            "malformed JWT: expected 3 segments".to_string(),
        ));
    }

    let payload_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[1])
        .map_err(|e| CodexAuthError::AuthJsonInvalid(format!("JWT payload decode: {e}")))?;

    let payload: serde_json::Value = serde_json::from_slice(&payload_bytes)
        .map_err(|e| CodexAuthError::AuthJsonInvalid(format!("JWT payload JSON: {e}")))?;

    // Extract chatgpt_account_id — top-level wins, then the namespaced OpenAI
    // Auth0 claim (`https://api.openai.com/auth`). This order mirrors Goose's
    // `account_id_from_claims` function.
    let chatgpt_account_id = if let Some(id) = payload
        .get("chatgpt_account_id")
        .and_then(|v| v.as_str())
        .map(str::to_owned)
    {
        Some(id)
    } else {
        payload
            .get("https://api.openai.com/auth")
            .and_then(|ns| ns.get("chatgpt_account_id"))
            .and_then(|v| v.as_str())
            .map(str::to_owned)
    };

    let exp = payload.get("exp").and_then(exp_from_value);

    Ok(ChatGptClaims {
        chatgpt_account_id,
        exp,
    })
}

fn exp_from_value(v: &serde_json::Value) -> Option<i64> {
    if let Some(exp) = v.as_i64() {
        return Some(exp);
    }

    let f = v.as_f64()?;
    if !f.is_finite() || f.fract() != 0.0 {
        return None;
    }
    format!("{f:.0}").parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_token(claims: &serde_json::Value) -> String {
        use base64::Engine as _;
        let payload_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(claims).unwrap());
        format!("header.{payload_b64}.signature")
    }

    #[test]
    fn decodes_top_level_chatgpt_account_id() {
        let token = make_token(&json!({"chatgpt_account_id": "acct-123"}));
        let claims = decode_chatgpt_claims(&token).unwrap();
        assert_eq!(claims.chatgpt_account_id, Some("acct-123".to_owned()));
    }

    #[test]
    fn decodes_nested_chatgpt_account_id_under_api_openai_com_auth() {
        let token =
            make_token(&json!({"https://api.openai.com/auth": {"chatgpt_account_id": "acct-456"}}));
        let claims = decode_chatgpt_claims(&token).unwrap();
        assert_eq!(claims.chatgpt_account_id, Some("acct-456".to_owned()));
    }

    #[test]
    fn top_level_wins_over_nested() {
        let token = make_token(&json!({
            "chatgpt_account_id": "top-level",
            "https://api.openai.com/auth": {"chatgpt_account_id": "nested"}
        }));
        let claims = decode_chatgpt_claims(&token).unwrap();
        assert_eq!(claims.chatgpt_account_id, Some("top-level".to_owned()));
    }

    #[test]
    fn returns_none_account_id_when_absent() {
        let token = make_token(&json!({"sub": "user-999"}));
        let claims = decode_chatgpt_claims(&token).unwrap();
        assert_eq!(claims.chatgpt_account_id, None);
    }

    #[test]
    fn decodes_exp_claim() {
        let token = make_token(&json!({"exp": 1_700_000_000_i64}));
        let claims = decode_chatgpt_claims(&token).unwrap();
        assert_eq!(claims.exp, Some(1_700_000_000_i64));
    }

    #[test]
    fn decodes_exp_claim_when_emitted_as_float() {
        let token = make_token(&json!({ "exp": 1_700_000_000.0 }));
        let claims = decode_chatgpt_claims(&token).unwrap();
        assert_eq!(claims.exp, Some(1_700_000_000));
    }

    #[test]
    fn returns_err_on_malformed_token() {
        let err = decode_chatgpt_claims("not.enough").unwrap_err();
        assert!(
            matches!(err, CodexAuthError::AuthJsonInvalid(_)),
            "expected AuthJsonInvalid, got: {err:?}"
        );
    }

    #[test]
    fn returns_err_on_bad_base64() {
        let err = decode_chatgpt_claims("header.!!!!.sig").unwrap_err();
        assert!(
            matches!(err, CodexAuthError::AuthJsonInvalid(_)),
            "expected AuthJsonInvalid, got: {err:?}"
        );
    }

    #[test]
    fn returns_err_on_payload_not_json() {
        use base64::Engine as _;
        let bad_payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b"not json");
        let token = format!("header.{bad_payload}.sig");
        let err = decode_chatgpt_claims(&token).unwrap_err();
        assert!(
            matches!(err, CodexAuthError::AuthJsonInvalid(_)),
            "expected AuthJsonInvalid, got: {err:?}"
        );
    }
}
