//! Domainless payload capability tags.

/// Opaque schema-declared capability tag.
///
/// Core stores and compares tags; it never interprets their meaning.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CapabilityTag(String);

impl CapabilityTag {
    /// Parse a capability tag.
    ///
    /// # Errors
    ///
    /// Returns [`CapabilityTagError::Invalid`] unless `raw` matches
    /// `^[a-z][a-z0-9-]*$`.
    pub fn parse(raw: impl Into<String>) -> Result<Self, CapabilityTagError> {
        let value = raw.into();
        if is_valid_capability_tag(&value) {
            Ok(Self(value))
        } else {
            Err(CapabilityTagError::Invalid { value })
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CapabilityTagError {
    #[error("invalid capability tag {value:?}: expected ^[a-z][a-z0-9-]*$")]
    Invalid { value: String },
}

fn is_valid_capability_tag(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_lowercase() {
        return false;
    }
    chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
}

#[cfg(test)]
mod tests {
    use super::CapabilityTag;

    #[test]
    fn accepts_lowercase_shared_vocab() {
        for raw in ["actor", "task", "shared-vocab", "a1-b2", "x-"] {
            let tag = CapabilityTag::parse(raw).expect("valid capability tag");
            assert_eq!(tag.as_str(), raw);
        }
    }

    #[test]
    fn rejects_prefixed_uppercase_slash_and_leading_digit() {
        for raw in [
            "",
            "proxima-code/actor",
            "Actor",
            "act/or",
            "1actor",
            "-actor",
        ] {
            assert!(
                CapabilityTag::parse(raw).is_err(),
                "{raw:?} must not parse as a capability tag",
            );
        }
    }
}
