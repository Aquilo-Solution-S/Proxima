//! Issuer-aware `(iss, sub) -> SubjectBinding` identity map for the default
//! Proxima MCP group-auth path.
//!
//! `OidcAuthConfig` carries no identity mapping (see `config.rs`): the
//! default [`crate::OidcAuthenticator::new`] path resolves the validated
//! `(iss, sub)` pair through this map instead. The map has two textual
//! encodings: an issuer-aware JSON array (the primary, unambiguous format)
//! and a `sub:uuid` shorthand that is valid only
//! when exactly one issuer can ever be accepted — with more than one issuer
//! accepted, a bare `sub` cannot disambiguate which issuer's token it
//! belongs to, so the shorthand is rejected at parse time rather than
//! silently binding to the wrong issuer.

use std::collections::HashMap;

use proxima_core::{MAX_OPERATOR_LABEL_CHARS, UserId};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum OidcSubjectMapError {
    #[error("subject map entry {index} has an empty {field}")]
    EmptyField { index: usize, field: &'static str },
    #[error("duplicate subject map entry for issuer {issuer:?} subject {subject:?}")]
    DuplicateEntry { issuer: String, subject: String },
    #[error("subject map entry {index} has an invalid user_id {value:?}: {reason}")]
    InvalidUserId {
        index: usize,
        value: String,
        reason: String,
    },
    #[error("invalid subject map JSON: {0}")]
    InvalidJson(String),
    #[error(
        "legacy sub-only subject map shorthand requires exactly one accepted issuer, got {count}"
    )]
    AmbiguousIssuerForShorthand { count: usize },
    #[error("shorthand subject map entry {index} is not \"sub:uuid\": {raw:?}")]
    MalformedShorthandEntry { index: usize, raw: String },
    #[error("subject map entry {index} has an invalid trusted_model_id {value:?}: {reason}")]
    InvalidTrustedModelId {
        index: usize,
        value: String,
        reason: String,
    },
}

/// `deny_unknown_fields` is load-bearing, not tidiness: a typo like
/// `"trusted_model"` would otherwise parse, bind nothing, and leave an
/// operator believing a runner is certified when every write it makes is
/// labelled by the caller. A misspelled key fails boot instead.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEntry {
    iss: String,
    sub: String,
    user_id: String,
    #[serde(default)]
    trusted_model_id: Option<String>,
}

/// What one configured `(iss, sub)` pair binds.
///
/// `trusted_model_id` is the deployment's statement that this principal *is*
/// a particular configured runner. It certifies which runner reached the
/// edge — not that a model produced any particular content — and is the only
/// model provenance a flavor may build policy on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubjectBinding {
    pub user_id: UserId,
    pub trusted_model_id: Option<String>,
}

impl SubjectBinding {
    /// A binding that names no runner. The common case: an ordinary human
    /// principal.
    #[must_use]
    pub const fn new(user_id: UserId) -> Self {
        Self {
            user_id,
            trusted_model_id: None,
        }
    }

    /// Bind a configured runner identity to this principal.
    #[must_use]
    pub fn with_trusted_model_id(mut self, trusted_model_id: impl Into<String>) -> Self {
        self.trusted_model_id = Some(trusted_model_id.into());
        self
    }
}

/// Issuer-aware `(iss, sub) -> SubjectBinding` map. Construction always
/// validates: no duplicate `(iss, sub)` keys, no empty fields, no invalid
/// UUIDs, and a `trusted_model_id` that is non-blank and within
/// [`MAX_OPERATOR_LABEL_CHARS`] when present.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OidcSubjectMap {
    entries: HashMap<(String, String), SubjectBinding>,
}

impl OidcSubjectMap {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Everything the configured `(iss, sub)` pair binds — the user and,
    /// when the deployment declares one, the trusted runner identity.
    #[must_use]
    pub fn resolve_binding(&self, issuer: &str, subject: &str) -> Option<SubjectBinding> {
        self.entries
            .get(&(issuer.to_string(), subject.to_string()))
            .cloned()
    }

    /// Identity-only view of [`Self::resolve_binding`], for call sites that
    /// answer "who is this" and nothing else.
    #[must_use]
    pub fn resolve(&self, issuer: &str, subject: &str) -> Option<UserId> {
        self.resolve_binding(issuer, subject)
            .map(|binding| binding.user_id)
    }

    /// # Errors
    ///
    /// Returns [`OidcSubjectMapError::EmptyField`] for an empty issuer or
    /// subject, or [`OidcSubjectMapError::DuplicateEntry`] when `(issuer,
    /// subject)` was already inserted.
    pub fn insert(
        &mut self,
        issuer: impl Into<String>,
        subject: impl Into<String>,
        user_id: UserId,
    ) -> Result<(), OidcSubjectMapError> {
        self.insert_binding(issuer, subject, SubjectBinding::new(user_id))
    }

    /// # Errors
    ///
    /// As [`Self::insert`], plus
    /// [`OidcSubjectMapError::InvalidTrustedModelId`] for a blank or
    /// over-long `trusted_model_id`.
    pub fn insert_binding(
        &mut self,
        issuer: impl Into<String>,
        subject: impl Into<String>,
        binding: SubjectBinding,
    ) -> Result<(), OidcSubjectMapError> {
        self.insert_at(0, issuer.into(), subject.into(), binding)
    }

    fn insert_at(
        &mut self,
        index: usize,
        issuer: String,
        subject: String,
        binding: SubjectBinding,
    ) -> Result<(), OidcSubjectMapError> {
        if issuer.is_empty() {
            return Err(OidcSubjectMapError::EmptyField {
                index,
                field: "iss",
            });
        }
        if subject.is_empty() {
            return Err(OidcSubjectMapError::EmptyField {
                index,
                field: "sub",
            });
        }
        let binding = SubjectBinding {
            user_id: binding.user_id,
            trusted_model_id: binding
                .trusted_model_id
                .map(|raw| validate_trusted_model_id(index, raw))
                .transpose()?,
        };
        let key = (issuer.clone(), subject.clone());
        if self.entries.contains_key(&key) {
            return Err(OidcSubjectMapError::DuplicateEntry { issuer, subject });
        }
        self.entries.insert(key, binding);
        Ok(())
    }

    /// Parse the issuer-aware JSON array format:
    /// `[{"iss":"https://issuer.example","sub":"subject","user_id":"<uuid>"}]`
    ///
    /// Each entry may add `"trusted_model_id":"<label>"` to declare that this
    /// principal is a configured runner; see [`SubjectBinding`].
    ///
    /// # Errors
    ///
    /// Returns [`OidcSubjectMapError::InvalidJson`] when `raw` does not
    /// parse, or the per-entry validation errors from [`Self::insert`].
    pub fn from_json(raw: &str) -> Result<Self, OidcSubjectMapError> {
        let raw_entries: Vec<RawEntry> = serde_json::from_str(raw)
            .map_err(|err| OidcSubjectMapError::InvalidJson(err.to_string()))?;
        let mut map = Self::new();
        for (index, entry) in raw_entries.into_iter().enumerate() {
            if entry.user_id.trim().is_empty() {
                return Err(OidcSubjectMapError::EmptyField {
                    index,
                    field: "user_id",
                });
            }
            let uuid = uuid::Uuid::parse_str(entry.user_id.trim()).map_err(|err| {
                OidcSubjectMapError::InvalidUserId {
                    index,
                    value: entry.user_id.clone(),
                    reason: err.to_string(),
                }
            })?;
            map.insert_at(
                index,
                entry.iss,
                entry.sub,
                SubjectBinding {
                    user_id: UserId::new(uuid),
                    trusted_model_id: entry.trusted_model_id,
                },
            )?;
        }
        Ok(map)
    }

    /// Parse the `sub:uuid,sub2:uuid2` shorthand. Every entry binds
    /// to `accepted_issuers[0]`.
    ///
    /// The shorthand has no field for a trusted model id and never yields
    /// one: a deployment that wants trusted model provenance uses the JSON
    /// form.
    ///
    /// # Errors
    ///
    /// Returns [`OidcSubjectMapError::AmbiguousIssuerForShorthand`] unless
    /// `accepted_issuers` has exactly one element, or a per-entry parse
    /// error otherwise.
    pub fn from_legacy_shorthand(
        raw: &str,
        accepted_issuers: &[String],
    ) -> Result<Self, OidcSubjectMapError> {
        if accepted_issuers.len() != 1 {
            return Err(OidcSubjectMapError::AmbiguousIssuerForShorthand {
                count: accepted_issuers.len(),
            });
        }
        let issuer = accepted_issuers[0].clone();
        let mut map = Self::new();
        for (index, raw_entry) in raw
            .split(',')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
            .enumerate()
        {
            let (subject, user_id_raw) = raw_entry.split_once(':').ok_or_else(|| {
                OidcSubjectMapError::MalformedShorthandEntry {
                    index,
                    raw: raw_entry.to_string(),
                }
            })?;
            if user_id_raw.trim().is_empty() {
                return Err(OidcSubjectMapError::EmptyField {
                    index,
                    field: "user_id",
                });
            }
            let uuid = uuid::Uuid::parse_str(user_id_raw.trim()).map_err(|err| {
                OidcSubjectMapError::InvalidUserId {
                    index,
                    value: user_id_raw.to_string(),
                    reason: err.to_string(),
                }
            })?;
            map.insert_at(
                index,
                issuer.clone(),
                subject.to_string(),
                SubjectBinding::new(UserId::new(uuid)),
            )?;
        }
        Ok(map)
    }
}

/// Trim, then bound. The label lands in append-only provenance and is
/// compared against the caller's own `model_id`, so the stored form and the
/// compared form must be the same string.
fn validate_trusted_model_id(index: usize, raw: String) -> Result<String, OidcSubjectMapError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(OidcSubjectMapError::InvalidTrustedModelId {
            index,
            value: raw,
            reason: "must not be blank".to_string(),
        });
    }
    if trimmed.chars().count() > MAX_OPERATOR_LABEL_CHARS {
        return Err(OidcSubjectMapError::InvalidTrustedModelId {
            index,
            value: raw,
            reason: format!("must be at most {MAX_OPERATOR_LABEL_CHARS} characters"),
        });
    }
    Ok(trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ISSUER_A: &str = "https://issuer-a.example";
    const ISSUER_B: &str = "https://issuer-b.example";

    fn uuid_str() -> String {
        uuid::Uuid::now_v7().to_string()
    }

    #[test]
    fn resolves_by_issuer_and_subject_pair() {
        let user_id = UserId::new(uuid::Uuid::now_v7());
        let mut map = OidcSubjectMap::new();
        map.insert(ISSUER_A, "subject-1", user_id).expect("insert");

        assert_eq!(map.resolve(ISSUER_A, "subject-1"), Some(user_id));
        assert_eq!(map.resolve(ISSUER_B, "subject-1"), None);
        assert_eq!(map.resolve(ISSUER_A, "subject-2"), None);
    }

    #[test]
    fn rejects_duplicate_issuer_subject_pair() {
        let mut map = OidcSubjectMap::new();
        map.insert(ISSUER_A, "subject-1", UserId::new(uuid::Uuid::now_v7()))
            .expect("first insert");

        let err = map
            .insert(ISSUER_A, "subject-1", UserId::new(uuid::Uuid::now_v7()))
            .expect_err("duplicate rejected");
        assert_eq!(
            err,
            OidcSubjectMapError::DuplicateEntry {
                issuer: ISSUER_A.to_string(),
                subject: "subject-1".to_string(),
            }
        );
    }

    #[test]
    fn rejects_empty_issuer_or_subject() {
        let mut map = OidcSubjectMap::new();
        assert!(matches!(
            map.insert("", "subject-1", UserId::new(uuid::Uuid::now_v7())),
            Err(OidcSubjectMapError::EmptyField { field: "iss", .. })
        ));
        assert!(matches!(
            map.insert(ISSUER_A, "", UserId::new(uuid::Uuid::now_v7())),
            Err(OidcSubjectMapError::EmptyField { field: "sub", .. })
        ));
    }

    #[test]
    fn from_json_parses_issuer_aware_entries() {
        let user_a = uuid_str();
        let user_b = uuid_str();
        let raw = format!(
            r#"[{{"iss":"{ISSUER_A}","sub":"subject-1","user_id":"{user_a}"}},
                {{"iss":"{ISSUER_B}","sub":"subject-1","user_id":"{user_b}"}}]"#
        );

        let map = OidcSubjectMap::from_json(&raw).expect("valid json");

        assert_eq!(map.len(), 2);
        assert_eq!(
            map.resolve(ISSUER_A, "subject-1"),
            Some(UserId::new(uuid::Uuid::parse_str(&user_a).unwrap()))
        );
        assert_eq!(
            map.resolve(ISSUER_B, "subject-1"),
            Some(UserId::new(uuid::Uuid::parse_str(&user_b).unwrap()))
        );
    }

    #[test]
    fn from_json_rejects_duplicate_entries() {
        let user_id = uuid_str();
        let raw = format!(
            r#"[{{"iss":"{ISSUER_A}","sub":"subject-1","user_id":"{user_id}"}},
                {{"iss":"{ISSUER_A}","sub":"subject-1","user_id":"{user_id}"}}]"#
        );

        assert!(matches!(
            OidcSubjectMap::from_json(&raw),
            Err(OidcSubjectMapError::DuplicateEntry { .. })
        ));
    }

    #[test]
    fn from_json_rejects_invalid_uuid() {
        let raw = format!(r#"[{{"iss":"{ISSUER_A}","sub":"subject-1","user_id":"not-a-uuid"}}]"#);

        assert!(matches!(
            OidcSubjectMap::from_json(&raw),
            Err(OidcSubjectMapError::InvalidUserId { index: 0, .. })
        ));
    }

    #[test]
    fn from_json_rejects_empty_subject() {
        let user_id = uuid_str();
        let raw = format!(r#"[{{"iss":"{ISSUER_A}","sub":"","user_id":"{user_id}"}}]"#);

        assert!(matches!(
            OidcSubjectMap::from_json(&raw),
            Err(OidcSubjectMapError::EmptyField {
                index: 0,
                field: "sub"
            })
        ));
    }

    #[test]
    fn from_json_binds_an_optional_trusted_model_id() {
        let plain = uuid_str();
        let runner = uuid_str();
        let raw = format!(
            r#"[{{"iss":"{ISSUER_A}","sub":"human","user_id":"{plain}"}},
                {{"iss":"{ISSUER_A}","sub":"runner","user_id":"{runner}",
                  "trusted_model_id":"acme/runner-v3"}}]"#
        );

        let map = OidcSubjectMap::from_json(&raw).expect("valid json");

        let human = map.resolve_binding(ISSUER_A, "human").expect("mapped");
        assert_eq!(
            human.user_id,
            UserId::new(uuid::Uuid::parse_str(&plain).unwrap())
        );
        assert_eq!(
            human.trusted_model_id, None,
            "an entry without the field binds no runner"
        );

        let bound = map.resolve_binding(ISSUER_A, "runner").expect("mapped");
        assert_eq!(bound.trusted_model_id.as_deref(), Some("acme/runner-v3"));

        assert_eq!(
            map.resolve(ISSUER_A, "runner"),
            Some(UserId::new(uuid::Uuid::parse_str(&runner).unwrap())),
            "the identity-only wrapper still answers"
        );
        assert_eq!(map.resolve_binding(ISSUER_A, "absent"), None);
    }

    #[test]
    fn from_json_trims_the_trusted_model_id() {
        let user_id = uuid_str();
        let raw = format!(
            r#"[{{"iss":"{ISSUER_A}","sub":"runner","user_id":"{user_id}",
                  "trusted_model_id":"  acme/runner-v3  "}}]"#
        );

        let map = OidcSubjectMap::from_json(&raw).expect("valid json");

        assert_eq!(
            map.resolve_binding(ISSUER_A, "runner")
                .expect("mapped")
                .trusted_model_id
                .as_deref(),
            Some("acme/runner-v3"),
            "the stored label and the compared label must be one string"
        );
    }

    #[test]
    fn from_json_rejects_a_blank_trusted_model_id() {
        let user_id = uuid_str();
        for blank in ["", "   "] {
            let raw = format!(
                r#"[{{"iss":"{ISSUER_A}","sub":"runner","user_id":"{user_id}",
                      "trusted_model_id":"{blank}"}}]"#
            );
            assert!(
                matches!(
                    OidcSubjectMap::from_json(&raw),
                    Err(OidcSubjectMapError::InvalidTrustedModelId { index: 0, .. })
                ),
                "blank {blank:?} must be rejected rather than stored"
            );
        }
    }

    #[test]
    fn from_json_bounds_the_trusted_model_id_at_the_operator_label_limit() {
        let user_id = uuid_str();
        let at_limit = "m".repeat(MAX_OPERATOR_LABEL_CHARS);
        let over_limit = "m".repeat(MAX_OPERATOR_LABEL_CHARS + 1);

        let raw = format!(
            r#"[{{"iss":"{ISSUER_A}","sub":"runner","user_id":"{user_id}",
                  "trusted_model_id":"{at_limit}"}}]"#
        );
        assert!(
            OidcSubjectMap::from_json(&raw).is_ok(),
            "the bound itself fits"
        );

        let raw = format!(
            r#"[{{"iss":"{ISSUER_A}","sub":"runner","user_id":"{user_id}",
                  "trusted_model_id":"{over_limit}"}}]"#
        );
        assert!(matches!(
            OidcSubjectMap::from_json(&raw),
            Err(OidcSubjectMapError::InvalidTrustedModelId { index: 0, .. })
        ));
    }

    /// A misspelled key must fail boot, not parse into "no runner bound".
    /// The silent version leaves an operator believing a runner is
    /// certified while every write it makes carries the caller's own label.
    #[test]
    fn from_json_rejects_an_unknown_entry_field() {
        let user_id = uuid_str();
        let raw = format!(
            r#"[{{"iss":"{ISSUER_A}","sub":"runner","user_id":"{user_id}",
                  "trusted_model":"acme/runner-v3"}}]"#
        );

        let err = OidcSubjectMap::from_json(&raw).expect_err("a typo must not parse");
        let OidcSubjectMapError::InvalidJson(message) = err else {
            panic!("an unknown field is a JSON shape error");
        };
        assert!(message.contains("trusted_model"), "{message}");
    }

    /// The message quotes the same constant the check uses, so the two
    /// cannot drift apart.
    #[test]
    fn the_over_long_message_states_the_shared_bound() {
        let user_id = uuid_str();
        let over_limit = "m".repeat(MAX_OPERATOR_LABEL_CHARS + 1);
        let raw = format!(
            r#"[{{"iss":"{ISSUER_A}","sub":"runner","user_id":"{user_id}",
                  "trusted_model_id":"{over_limit}"}}]"#
        );

        let err = OidcSubjectMap::from_json(&raw).expect_err("over-long is rejected");
        assert!(
            err.to_string()
                .contains(&format!("at most {MAX_OPERATOR_LABEL_CHARS} characters")),
            "{err}"
        );
    }

    #[test]
    fn programmatic_insert_validates_the_trusted_model_id_too() {
        let mut map = OidcSubjectMap::new();
        assert!(matches!(
            map.insert_binding(
                ISSUER_A,
                "runner",
                SubjectBinding::new(UserId::new(uuid::Uuid::now_v7())).with_trusted_model_id("   "),
            ),
            Err(OidcSubjectMapError::InvalidTrustedModelId { .. })
        ));
    }

    /// The shorthand has no field for it, so it can never certify a runner.
    #[test]
    fn legacy_shorthand_never_yields_a_trusted_model_id() {
        let raw = format!("subject-1:{}", uuid_str());
        let map = OidcSubjectMap::from_legacy_shorthand(&raw, &[ISSUER_A.to_string()])
            .expect("single-issuer shorthand accepted");

        assert_eq!(
            map.resolve_binding(ISSUER_A, "subject-1")
                .expect("mapped")
                .trusted_model_id,
            None
        );
    }

    /// `insert` is the identity-only door; it binds no runner.
    #[test]
    fn plain_insert_binds_no_trusted_model_id() {
        let mut map = OidcSubjectMap::new();
        map.insert(ISSUER_A, "subject-1", UserId::new(uuid::Uuid::now_v7()))
            .expect("insert");
        assert_eq!(
            map.resolve_binding(ISSUER_A, "subject-1")
                .expect("mapped")
                .trusted_model_id,
            None
        );
    }

    #[test]
    fn from_json_rejects_malformed_json() {
        assert!(matches!(
            OidcSubjectMap::from_json("not json"),
            Err(OidcSubjectMapError::InvalidJson(_))
        ));
    }

    #[test]
    fn legacy_shorthand_binds_every_entry_to_the_sole_accepted_issuer() {
        let user_a = uuid_str();
        let user_b = uuid_str();
        let raw = format!("subject-1:{user_a},subject-2:{user_b}");

        let map = OidcSubjectMap::from_legacy_shorthand(&raw, &[ISSUER_A.to_string()])
            .expect("single-issuer shorthand accepted");

        assert_eq!(
            map.resolve(ISSUER_A, "subject-1"),
            Some(UserId::new(uuid::Uuid::parse_str(&user_a).unwrap()))
        );
        assert_eq!(
            map.resolve(ISSUER_A, "subject-2"),
            Some(UserId::new(uuid::Uuid::parse_str(&user_b).unwrap()))
        );
    }

    #[test]
    fn sub_only_subject_map_is_rejected_when_multiple_issuers_are_configured() {
        let raw = format!("subject-1:{}", uuid_str());

        let err = OidcSubjectMap::from_legacy_shorthand(
            &raw,
            &[ISSUER_A.to_string(), ISSUER_B.to_string()],
        )
        .expect_err("ambiguous issuer must be rejected");

        assert_eq!(
            err,
            OidcSubjectMapError::AmbiguousIssuerForShorthand { count: 2 }
        );
    }

    #[test]
    fn legacy_shorthand_rejects_zero_accepted_issuers() {
        let raw = format!("subject-1:{}", uuid_str());

        let err = OidcSubjectMap::from_legacy_shorthand(&raw, &[])
            .expect_err("zero issuers is also ambiguous");

        assert_eq!(
            err,
            OidcSubjectMapError::AmbiguousIssuerForShorthand { count: 0 }
        );
    }

    #[test]
    fn legacy_shorthand_rejects_malformed_entry() {
        let err = OidcSubjectMap::from_legacy_shorthand("subject-1", &[ISSUER_A.to_string()])
            .expect_err("missing colon is malformed");
        assert!(matches!(
            err,
            OidcSubjectMapError::MalformedShorthandEntry { index: 0, .. }
        ));
    }

    #[test]
    fn legacy_shorthand_rejects_invalid_uuid() {
        let err =
            OidcSubjectMap::from_legacy_shorthand("subject-1:not-a-uuid", &[ISSUER_A.to_string()])
                .expect_err("invalid uuid rejected");
        assert!(matches!(
            err,
            OidcSubjectMapError::InvalidUserId { index: 0, .. }
        ));
    }

    #[test]
    fn legacy_shorthand_rejects_duplicate_subjects() {
        let user_id = uuid_str();
        let raw = format!("subject-1:{user_id},subject-1:{user_id}");
        let err = OidcSubjectMap::from_legacy_shorthand(&raw, &[ISSUER_A.to_string()])
            .expect_err("duplicate subject rejected");
        assert!(matches!(err, OidcSubjectMapError::DuplicateEntry { .. }));
    }
}
