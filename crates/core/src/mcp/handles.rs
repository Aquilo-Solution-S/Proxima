//! Canonical MCP wire references: typed prefixed UUIDs.
//!
//! Every entity crossing the MCP boundary is referenced by exactly one
//! grammar — `<prefix>:<uuid>` with a class-specific prefix (`F`, `A`,
//! `P`, `G`, `E`; flavors register their own uppercase prefixes). Parsing
//! validates the class, so a Fact argument can never silently accept an
//! Abstraction reference.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MemoryHandleClass {
    Fact,
    Abstraction,
    Perspective,
}

impl MemoryHandleClass {
    #[must_use]
    pub const fn prefix(self) -> char {
        match self {
            Self::Fact => 'F',
            Self::Abstraction => 'A',
            Self::Perspective => 'P',
        }
    }

    #[must_use]
    pub fn from_memory_kind(kind: &str) -> Option<Self> {
        match kind {
            "Fact" | "fact" => Some(Self::Fact),
            "Abstraction" | "abstraction" => Some(Self::Abstraction),
            "Perspective" | "perspective" => Some(Self::Perspective),
            _ => None,
        }
    }
}

impl std::fmt::Display for MemoryHandleClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Fact => write!(f, "Fact"),
            Self::Abstraction => write!(f, "Abstraction"),
            Self::Perspective => write!(f, "Perspective"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PrefixedUuidClass {
    Fact,
    Abstraction,
    Perspective,
    Edge,
    Goal,
}

impl PrefixedUuidClass {
    #[must_use]
    pub const fn prefix(self) -> char {
        match self {
            Self::Fact => MemoryHandleClass::Fact.prefix(),
            Self::Abstraction => MemoryHandleClass::Abstraction.prefix(),
            Self::Perspective => MemoryHandleClass::Perspective.prefix(),
            Self::Edge => 'E',
            Self::Goal => 'G',
        }
    }
}

impl std::fmt::Display for PrefixedUuidClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Fact => write!(f, "Fact"),
            Self::Abstraction => write!(f, "Abstraction"),
            Self::Perspective => write!(f, "Perspective"),
            Self::Edge => write!(f, "Edge"),
            Self::Goal => write!(f, "Goal"),
        }
    }
}

impl From<MemoryHandleClass> for PrefixedUuidClass {
    fn from(value: MemoryHandleClass) -> Self {
        match value {
            MemoryHandleClass::Fact => Self::Fact,
            MemoryHandleClass::Abstraction => Self::Abstraction,
            MemoryHandleClass::Perspective => Self::Perspective,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PrefixedUuidError {
    #[error("malformed {expected} id '{input}': expected {prefix}:<uuid>")]
    Malformed {
        input: String,
        expected: PrefixedUuidClass,
        prefix: char,
    },
    #[error(
        "expected {expected} id ({expected_prefix}:<uuid>), got prefix '{actual_prefix}' in '{input}'"
    )]
    WrongPrefix {
        input: String,
        expected: PrefixedUuidClass,
        expected_prefix: char,
        actual_prefix: String,
    },
    #[error("invalid uuid in {expected} id '{input}': {source}")]
    InvalidUuid {
        input: String,
        expected: PrefixedUuidClass,
        source: uuid::Error,
    },
}

#[must_use]
pub fn format_prefixed_uuid(id: uuid::Uuid, class: PrefixedUuidClass) -> String {
    format!("{}:{id}", class.prefix())
}

/// Parse a prefixed wire UUID and validate the expected entity class.
///
/// # Errors
///
/// Returns [`PrefixedUuidError`] when the value is not `<prefix>:<uuid>`,
/// the prefix names another entity class, or the UUID body is malformed.
pub fn parse_prefixed_uuid(
    raw: &str,
    expected: PrefixedUuidClass,
) -> Result<uuid::Uuid, PrefixedUuidError> {
    let expected_prefix = expected.prefix();
    let Some((actual_prefix, uuid_part)) = raw.split_once(':') else {
        return Err(PrefixedUuidError::Malformed {
            input: raw.to_string(),
            expected,
            prefix: expected_prefix,
        });
    };
    let mut prefix_chars = actual_prefix.chars();
    if prefix_chars.next() != Some(expected_prefix) || prefix_chars.next().is_some() {
        return Err(PrefixedUuidError::WrongPrefix {
            input: raw.to_string(),
            expected,
            expected_prefix,
            actual_prefix: actual_prefix.to_string(),
        });
    }
    uuid::Uuid::parse_str(uuid_part).map_err(|source| PrefixedUuidError::InvalidUuid {
        input: raw.to_string(),
        expected,
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn prefixed_uuid_format_parse_round_trips_per_class() {
        for class in [
            PrefixedUuidClass::Fact,
            PrefixedUuidClass::Abstraction,
            PrefixedUuidClass::Perspective,
            PrefixedUuidClass::Goal,
            PrefixedUuidClass::Edge,
        ] {
            let id = Uuid::now_v7();
            let raw = format_prefixed_uuid(id, class);
            assert_eq!(raw, format!("{}:{id}", class.prefix()));
            assert_eq!(parse_prefixed_uuid(&raw, class).expect("round trip"), id);
        }
    }

    #[test]
    fn prefixed_uuid_parse_rejects_wrong_prefix() {
        let id = Uuid::now_v7();
        let raw = format!("A:{id}");
        let err = parse_prefixed_uuid(&raw, PrefixedUuidClass::Fact).unwrap_err();
        assert!(err.to_string().contains("expected Fact id"));
        assert!(err.to_string().contains("got prefix 'A'"));
    }

    #[test]
    fn prefixed_uuid_parse_rejects_malformed_values() {
        for raw in [
            "not-a-prefixed-uuid",
            "F:",
            "F:not-a-uuid",
            "FF:00000000-0000-0000-0000-000000000000",
        ] {
            let err = parse_prefixed_uuid(raw, PrefixedUuidClass::Fact).unwrap_err();
            let msg = err.to_string();
            assert!(
                msg.contains("malformed Fact id")
                    || msg.contains("invalid uuid in Fact id")
                    || msg.contains("expected Fact id"),
                "message: {msg}"
            );
        }
    }
}
