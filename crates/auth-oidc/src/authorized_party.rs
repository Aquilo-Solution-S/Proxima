//! Authorized-party policy: which clients of one issuer may call one binding.

use std::collections::HashSet;

use serde_json::Value;

use crate::authenticator::OidcRejection;

/// Why [`AuthorizedPartyPolicy::new`] refused a set of client ids.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum AuthorizedPartyPolicyError {
    #[error("authorized-party policy needs at least one client id")]
    Empty,
    #[error("authorized-party policy client ids must not be blank")]
    Blank,
}

/// The clients (`azp`) allowed to call one binding: a non-empty set of
/// non-blank client ids, matched exactly and case-sensitively.
///
/// The policy belongs to one binding, so it is specific to that binding's
/// issuer and audience. It can only refuse a token the validator accepted; it
/// grants nothing and maps no identity. Attach it with
/// [`crate::OidcTokenValidator::with_authorized_party_policy`],
/// [`crate::OidcBinding::with_authorized_party_policy`] or
/// [`crate::OidcAuthenticator::with_authorized_party_policy`].
///
/// A token is refused when
///
/// - `azp` is present and not a string ([`OidcRejection::InvalidClaim`]),
/// - `aud` holds more than one distinct value and `azp` is absent
///   ([`OidcRejection::MissingAuthorizedParty`]), or
/// - `azp` is not in the set ([`OidcRejection::UnauthorizedParty`]).
///
/// Anything else is accepted: OIDC Core makes `azp` optional for a token with
/// a single audience.
///
/// The constructor is the only way in; a literal cannot skip its checks:
///
/// ```compile_fail,E0451
/// let _ = proxima_auth_oidc::AuthorizedPartyPolicy {
///     allowed: std::collections::HashSet::new(),
/// };
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizedPartyPolicy {
    allowed: HashSet<String>,
}

impl AuthorizedPartyPolicy {
    /// # Errors
    ///
    /// [`AuthorizedPartyPolicyError::Empty`] when `allowed` yields no client
    /// id, [`AuthorizedPartyPolicyError::Blank`] when one is empty or only
    /// whitespace.
    pub fn new(
        allowed: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<Self, AuthorizedPartyPolicyError> {
        let allowed = allowed.into_iter().map(Into::into).collect::<HashSet<_>>();
        if allowed.is_empty() {
            return Err(AuthorizedPartyPolicyError::Empty);
        }
        if allowed.iter().any(|client| client.trim().is_empty()) {
            return Err(AuthorizedPartyPolicyError::Blank);
        }
        Ok(Self { allowed })
    }

    /// Apply the policy to the claims of a token whose signature, `iss`,
    /// `aud`, `exp` and `nbf` have already been verified.
    pub(crate) fn check(&self, verified: &Value) -> Result<(), OidcRejection> {
        match verified.get("azp") {
            None if distinct_audiences(verified)? > 1 => Err(OidcRejection::MissingAuthorizedParty),
            None => Ok(()),
            Some(Value::String(azp)) if self.allowed.contains(azp) => Ok(()),
            Some(Value::String(azp)) => Err(OidcRejection::UnauthorizedParty { azp: azp.clone() }),
            Some(_) => Err(OidcRejection::InvalidClaim("azp".into())),
        }
    }
}

/// How many distinct values the verified `aud` holds: one string, or an array
/// of strings. Anything else is a malformed claim the policy cannot judge, so
/// it refuses.
fn distinct_audiences(verified: &Value) -> Result<usize, OidcRejection> {
    match verified.get("aud") {
        Some(Value::String(_)) => Ok(1),
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .ok_or_else(|| OidcRejection::InvalidClaim("aud".into()))
            })
            .collect::<Result<HashSet<_>, _>>()
            .map(|distinct| distinct.len()),
        _ => Err(OidcRejection::InvalidClaim("aud".into())),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn policy() -> AuthorizedPartyPolicy {
        AuthorizedPartyPolicy::new(["web", "cli"]).expect("policy")
    }

    #[test]
    fn the_constructor_refuses_an_empty_or_blank_set() {
        assert_eq!(
            AuthorizedPartyPolicy::new(Vec::<String>::new()),
            Err(AuthorizedPartyPolicyError::Empty)
        );
        for blank in ["", " ", "\t\n"] {
            assert_eq!(
                AuthorizedPartyPolicy::new(["web", blank]),
                Err(AuthorizedPartyPolicyError::Blank),
                "{blank:?} must be refused"
            );
        }
        let padded = AuthorizedPartyPolicy::new([" web "]).expect("padding is part of the id");
        assert_eq!(padded.check(&json!({"aud": "api", "azp": " web "})), Ok(()));
        assert!(padded.check(&json!({"aud": "api", "azp": "web"})).is_err());
    }

    #[test]
    fn matching_is_exact_and_case_sensitive() {
        let policy = policy();

        assert_eq!(policy.check(&json!({"aud": "api", "azp": "web"})), Ok(()));
        for other in ["WEB", "web ", " web", "we", "webs", ""] {
            assert_eq!(
                policy.check(&json!({"aud": "api", "azp": other})),
                Err(OidcRejection::UnauthorizedParty { azp: other.into() }),
                "{other:?} is not in the set"
            );
        }
    }

    #[test]
    fn a_single_audience_without_azp_passes_and_several_need_one() {
        let policy = policy();

        assert_eq!(policy.check(&json!({"aud": "api"})), Ok(()));
        assert_eq!(policy.check(&json!({"aud": ["api"]})), Ok(()));
        assert_eq!(
            policy.check(&json!({"aud": ["api", "api"]})),
            Ok(()),
            "a repeated value is one audience"
        );
        assert_eq!(
            policy.check(&json!({"aud": ["api", "other"]})),
            Err(OidcRejection::MissingAuthorizedParty)
        );
        assert_eq!(
            policy.check(&json!({"aud": ["api", "other"], "azp": "cli"})),
            Ok(())
        );
        assert_eq!(
            policy.check(&json!({"aud": ["api", "other"], "azp": "mobile"})),
            Err(OidcRejection::UnauthorizedParty {
                azp: "mobile".into()
            })
        );
    }

    #[test]
    fn a_present_azp_must_be_a_string() {
        let policy = policy();

        for azp in [
            json!(null),
            json!(7),
            json!(["web"]),
            json!({"web": 1}),
            json!(true),
        ] {
            assert_eq!(
                policy.check(&json!({"aud": "api", "azp": azp})),
                Err(OidcRejection::InvalidClaim("azp".into())),
                "{azp} is not a string"
            );
        }
    }

    #[test]
    fn an_audience_the_policy_cannot_read_is_refused() {
        let policy = policy();

        for aud in [json!(null), json!(7), json!(["api", 7]), json!({})] {
            assert_eq!(
                policy.check(&json!({"aud": aud})),
                Err(OidcRejection::InvalidClaim("aud".into())),
                "{aud} is not an audience"
            );
        }
        assert_eq!(
            policy.check(&json!({})),
            Err(OidcRejection::InvalidClaim("aud".into()))
        );
    }
}
