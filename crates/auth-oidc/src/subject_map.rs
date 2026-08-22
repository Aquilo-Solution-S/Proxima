//! Issuer-aware `(iss, sub) -> UserId` identity map for the default
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

use proxima_core::UserId;

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
}

#[derive(Debug, serde::Deserialize)]
struct RawEntry {
    iss: String,
    sub: String,
    user_id: String,
}

/// Issuer-aware `(iss, sub) -> UserId` map. Construction always validates:
/// no duplicate `(iss, sub)` keys, no empty fields, no invalid UUIDs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OidcSubjectMap {
    entries: HashMap<(String, String), UserId>,
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

    #[must_use]
    pub fn resolve(&self, issuer: &str, subject: &str) -> Option<UserId> {
        self.entries
            .get(&(issuer.to_string(), subject.to_string()))
            .copied()
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
        self.insert_at(0, issuer.into(), subject.into(), user_id)
    }

    fn insert_at(
        &mut self,
        index: usize,
        issuer: String,
        subject: String,
        user_id: UserId,
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
        let key = (issuer.clone(), subject.clone());
        if self.entries.contains_key(&key) {
            return Err(OidcSubjectMapError::DuplicateEntry { issuer, subject });
        }
        self.entries.insert(key, user_id);
        Ok(())
    }

    /// Parse the issuer-aware JSON array format:
    /// `[{"iss":"https://issuer.example","sub":"subject","user_id":"<uuid>"}]`
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
            map.insert_at(index, entry.iss, entry.sub, UserId::new(uuid))?;
        }
        Ok(map)
    }

    /// Parse the `sub:uuid,sub2:uuid2` shorthand. Every entry binds
    /// to `accepted_issuers[0]`.
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
                UserId::new(uuid),
            )?;
        }
        Ok(map)
    }
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
