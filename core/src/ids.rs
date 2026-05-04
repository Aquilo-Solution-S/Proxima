//! Core ID newtypes.
//!
//! See docs/07-storage.md §"ID types" for semantics.

use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct UserId(Uuid);

impl UserId {
    pub const fn new(inner: Uuid) -> Self {
        Self(inner)
    }

    pub const fn into_inner(self) -> Uuid {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct GroupId(Uuid);

impl GroupId {
    pub const fn new(inner: Uuid) -> Self {
        Self(inner)
    }

    pub const fn into_inner(self) -> Uuid {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct OrgId(Uuid);

impl OrgId {
    pub const fn new(inner: Uuid) -> Self {
        Self(inner)
    }

    pub const fn into_inner(self) -> Uuid {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct MemoryId(Uuid);

impl MemoryId {
    pub const fn new(inner: Uuid) -> Self {
        Self(inner)
    }

    pub const fn into_inner(self) -> Uuid {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct GoalId(Uuid);

impl GoalId {
    pub const fn new(inner: Uuid) -> Self {
        Self(inner)
    }

    pub const fn into_inner(self) -> Uuid {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct SchemaId(String);

impl SchemaId {
    pub const fn new(inner: String) -> Self {
        Self(inner)
    }

    pub fn into_inner(self) -> String {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct SchemaVersion(u32);

impl SchemaVersion {
    pub const fn new(inner: u32) -> Self {
        Self(inner)
    }

    pub const fn into_inner(self) -> u32 {
        self.0
    }
}
