//! Core ID newtypes.
//!
//! See docs/07-storage.md §"ID types" for semantics.

use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct UserId(Uuid);

impl UserId {
    #[must_use]
    pub const fn new(inner: Uuid) -> Self {
        Self(inner)
    }

    #[must_use]
    pub const fn into_inner(self) -> Uuid {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct GroupId(Uuid);

impl GroupId {
    #[must_use]
    pub const fn new(inner: Uuid) -> Self {
        Self(inner)
    }

    #[must_use]
    pub const fn into_inner(self) -> Uuid {
        self.0
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct MemoryId(Uuid);

impl MemoryId {
    #[must_use]
    pub const fn new(inner: Uuid) -> Self {
        Self(inner)
    }

    #[must_use]
    pub const fn into_inner(self) -> Uuid {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct FactEntityId(Uuid);

impl FactEntityId {
    #[must_use]
    pub const fn new(inner: Uuid) -> Self {
        Self(inner)
    }

    #[must_use]
    pub const fn into_inner(self) -> Uuid {
        self.0
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct GoalId(Uuid);

impl GoalId {
    #[must_use]
    pub const fn new(inner: Uuid) -> Self {
        Self(inner)
    }

    #[must_use]
    pub const fn into_inner(self) -> Uuid {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct EdgeId(Uuid);

impl EdgeId {
    #[must_use]
    pub const fn new(inner: Uuid) -> Self {
        Self(inner)
    }

    #[must_use]
    pub const fn into_inner(self) -> Uuid {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct SchemaId(String);

impl SchemaId {
    #[must_use]
    pub const fn new(inner: String) -> Self {
        Self(inner)
    }

    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SchemaId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct SchemaVersion(u32);

impl SchemaVersion {
    #[must_use]
    pub const fn new(inner: u32) -> Self {
        Self(inner)
    }

    #[must_use]
    pub const fn into_inner(self) -> u32 {
        self.0
    }
}

impl std::fmt::Display for SchemaVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// BLAKE3-32 `ContentHash` of (`source_id`, owner, payload).
/// Per docs/07 §"ID types" — events use `ContentHash` for
/// re-receipt dedup, not `UUIDv7`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct FactReceiptId([u8; 32]);

impl FactReceiptId {
    #[must_use]
    pub const fn new(inner: [u8; 32]) -> Self {
        Self(inner)
    }

    #[must_use]
    pub const fn into_inner(self) -> [u8; 32] {
        self.0
    }
}

/// `UUIDv7`, declared by the source at emit time.
/// See docs/01 §"The contract".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct SourceBatchId(uuid::Uuid);

impl SourceBatchId {
    #[must_use]
    pub const fn new(inner: uuid::Uuid) -> Self {
        Self(inner)
    }

    #[must_use]
    pub const fn into_inner(self) -> uuid::Uuid {
        self.0
    }
}

/// Stable identifier of an Event Source. docs/07 §"ID types".
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct SourceId(String);

impl SourceId {
    pub fn new(inner: impl Into<String>) -> Self {
        Self(inner.into())
    }

    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

// GoalWrite verb newtypes.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct OperatorId(uuid::Uuid);

impl OperatorId {
    #[must_use]
    pub const fn new(inner: uuid::Uuid) -> Self {
        Self(inner)
    }

    #[must_use]
    pub const fn into_inner(self) -> uuid::Uuid {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct InputContractId(uuid::Uuid);

impl InputContractId {
    #[must_use]
    pub const fn new(inner: uuid::Uuid) -> Self {
        Self(inner)
    }

    #[must_use]
    pub const fn into_inner(self) -> uuid::Uuid {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct ToolId(String);

impl ToolId {
    pub fn new(inner: impl Into<String>) -> Self {
        Self(inner.into())
    }

    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct ModelId(String);

impl ModelId {
    pub fn new(inner: impl Into<String>) -> Self {
        Self(inner.into())
    }

    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct PromptVersion(String);

impl PromptVersion {
    pub fn new(inner: impl Into<String>) -> Self {
        Self(inner.into())
    }

    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
