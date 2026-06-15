//! Opaque source-decoded cursor.
//!
//! Sources own the encoded format (json, bincode, sha, etc.). The
//! engine treats the bytes as opaque and round-trips them verbatim.
//!
//! v1 keeps the cursor in-memory at the call site (bin or test);
//! persistence to a `source_cursors` table is a follow-up once
//! restart-recovery or multi-process coordination is needed.

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Cursor(Vec<u8>);

impl Cursor {
    #[must_use]
    pub const fn empty() -> Self {
        Self(Vec::new())
    }

    #[must_use]
    pub const fn from_bytes(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}
