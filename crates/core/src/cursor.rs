//! Opaque source-decoded cursor.
//!
//! Sources own the encoded format. The engine treats the bytes as
//! opaque and round-trips them verbatim.
//!
//! Cursor persistence lives in the owner-scoped `source_cursors` table
//! for restart recovery. The persistence layer stores and returns the
//! same bytes verbatim; Proxima never interprets, validates, decodes,
//! normalizes, or derives ordering from them.

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
