pub mod acceptance;
pub mod code_chunk;
pub mod commit;
pub mod commit_summary;
pub mod development_perspective;
pub mod edge_calls;
pub mod execution_request;
pub mod file_revision;
pub mod personality_self;
pub mod test_request;

/// Serde adapter for 32-byte content hashes that round-trips through
/// Postgres `bytea` (rendered as `"\xDEADBEEF..."` by `row_to_json`)
/// while staying compact in binary serdes.
pub(crate) mod content_hash_serde {
    use serde::Serializer;
    use serde::de::{self, Deserializer, SeqAccess, Visitor};
    use std::fmt;

    pub fn serialize<S: Serializer>(bytes: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        if s.is_human_readable() {
            use std::fmt::Write;
            let mut hex = String::with_capacity(64);
            for b in bytes {
                let _ = write!(hex, "{b:02x}");
            }
            s.serialize_str(&hex)
        } else {
            s.serialize_bytes(bytes)
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = [u8; 32];
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("32-byte content hash (hex string or byte array)")
            }
            fn visit_str<E: de::Error>(self, s: &str) -> Result<[u8; 32], E> {
                let trimmed = s.strip_prefix("\\x").unwrap_or(s);
                if trimmed.len() != 64 {
                    return Err(E::custom(format!(
                        "expected 64 hex chars, got {}",
                        trimmed.len()
                    )));
                }
                let mut out = [0u8; 32];
                for (i, b) in out.iter_mut().enumerate() {
                    *b = u8::from_str_radix(&trimmed[i * 2..i * 2 + 2], 16)
                        .map_err(|e| E::custom(format!("hex: {e}")))?;
                }
                Ok(out)
            }
            fn visit_borrowed_str<E: de::Error>(self, s: &'de str) -> Result<[u8; 32], E> {
                self.visit_str(s)
            }
            fn visit_string<E: de::Error>(self, s: String) -> Result<[u8; 32], E> {
                self.visit_str(&s)
            }
            fn visit_bytes<E: de::Error>(self, b: &[u8]) -> Result<[u8; 32], E> {
                if b.len() != 32 {
                    return Err(E::custom(format!("expected 32 bytes, got {}", b.len())));
                }
                let mut out = [0u8; 32];
                out.copy_from_slice(b);
                Ok(out)
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<[u8; 32], A::Error> {
                let mut out = [0u8; 32];
                for (i, b) in out.iter_mut().enumerate() {
                    *b = seq
                        .next_element()?
                        .ok_or_else(|| de::Error::invalid_length(i, &self))?;
                }
                Ok(out)
            }
        }
        d.deserialize_any(V)
    }
}

pub use acceptance::{
    AcceptanceCriteriaV1, AcceptanceCriterionV1, AcceptanceVerifierKind, AcceptanceVerifierSpecV1,
};
pub use code_chunk::CodeChunkV1;
pub use commit::CommitV1;
pub use commit_summary::CommitSummaryV1;
pub use development_perspective::CodeDevelopmentPerspectiveV1;
pub use edge_calls::EdgeCallsV1;
pub use execution_request::ExecutionRequestV1;
pub use file_revision::{FileRevisionV1, FileState};
pub use personality_self::{CodeCommitSummarizerSelfV1, CodeEngineerSelfV1};
pub use test_request::TestRequestV1;
