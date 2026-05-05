//! Core ID newtypes.
//!
//! See docs/07-storage.md §"ID types" for semantics.

use uuid::Uuid;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, specta::Type,
)]
pub struct UserId(Uuid);

impl UserId {
    pub const fn new(inner: Uuid) -> Self {
        Self(inner)
    }

    pub const fn into_inner(self) -> Uuid {
        self.0
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, specta::Type,
)]
pub struct GroupId(Uuid);

impl GroupId {
    pub const fn new(inner: Uuid) -> Self {
        Self(inner)
    }

    pub const fn into_inner(self) -> Uuid {
        self.0
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, specta::Type,
)]
pub struct OrgId(Uuid);

impl OrgId {
    pub const fn new(inner: Uuid) -> Self {
        Self(inner)
    }

    pub const fn into_inner(self) -> Uuid {
        self.0
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, specta::Type,
)]
pub struct MemoryId(Uuid);

impl MemoryId {
    pub const fn new(inner: Uuid) -> Self {
        Self(inner)
    }

    pub const fn into_inner(self) -> Uuid {
        self.0
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, specta::Type,
)]
pub struct GoalId(Uuid);

impl GoalId {
    pub const fn new(inner: Uuid) -> Self {
        Self(inner)
    }

    pub const fn into_inner(self) -> Uuid {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct SchemaId(String);

impl SchemaId {
    pub const fn new(inner: String) -> Self {
        Self(inner)
    }

    pub fn into_inner(self) -> String {
        self.0
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, specta::Type,
)]
pub struct SchemaVersion(u32);

impl SchemaVersion {
    pub const fn new(inner: u32) -> Self {
        Self(inner)
    }

    pub const fn into_inner(self) -> u32 {
        self.0
    }
}

/// BLAKE3-32 ContentHash of (source_id, owner, payload).
/// Per docs/07 §"ID types" — events use ContentHash for
/// re-receipt dedup, not UUIDv7.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, specta::Type,
)]
pub struct EventId([u8; 32]);

impl EventId {
    pub const fn new(inner: [u8; 32]) -> Self {
        Self(inner)
    }

    pub const fn into_inner(self) -> [u8; 32] {
        self.0
    }

    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// UUIDv7, declared by the source at emit time.
/// See docs/01 §"The contract".
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, specta::Type,
)]
pub struct SourceBatchId(uuid::Uuid);

impl SourceBatchId {
    pub const fn new(inner: uuid::Uuid) -> Self {
        Self(inner)
    }

    pub const fn into_inner(self) -> uuid::Uuid {
        self.0
    }
}

/// Stable identifier of an Event Source. docs/07 §"ID types".
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct SourceId(String);

impl SourceId {
    pub fn new(inner: impl Into<String>) -> Self {
        Self(inner.into())
    }

    pub fn into_inner(self) -> String {
        self.0
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

// M2.5 — GoalWrite verb newtypes.

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, specta::Type,
)]
pub struct OperatorId(uuid::Uuid);

impl OperatorId {
    pub const fn new(inner: uuid::Uuid) -> Self {
        Self(inner)
    }

    pub const fn into_inner(self) -> uuid::Uuid {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct ToolId(String);

impl ToolId {
    pub fn new(inner: impl Into<String>) -> Self {
        Self(inner.into())
    }

    pub fn into_inner(self) -> String {
        self.0
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct ModelId(String);

impl ModelId {
    pub fn new(inner: impl Into<String>) -> Self {
        Self(inner.into())
    }

    pub fn into_inner(self) -> String {
        self.0
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct PromptVersion(String);

impl PromptVersion {
    pub fn new(inner: impl Into<String>) -> Self {
        Self(inner.into())
    }

    pub fn into_inner(self) -> String {
        self.0
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct PersonalityId(String);

impl PersonalityId {
    pub fn new(inner: impl Into<String>) -> Self {
        Self(inner.into())
    }

    pub fn into_inner(self) -> String {
        self.0
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// BLAKE3-32 hash of personality state for reproducibility.
/// Same shape as EventId (docs/07 §"ID types").
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, specta::Type,
)]
pub struct PersonalityStateHash([u8; 32]);

impl PersonalityStateHash {
    pub const fn new(inner: [u8; 32]) -> Self {
        Self(inner)
    }

    pub const fn into_inner(self) -> [u8; 32] {
        self.0
    }

    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}
