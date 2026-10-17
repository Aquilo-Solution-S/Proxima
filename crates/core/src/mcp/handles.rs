//! Semantic memory layers resolved from stored row kinds.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MemoryHandleClass {
    Fact,
    Abstraction,
    Perspective,
}

impl MemoryHandleClass {
    #[must_use]
    pub fn from_memory_kind(kind: &str) -> Option<Self> {
        match kind {
            "Fact" | "fact" => Some(Self::Fact),
            "Abstraction" | "abstraction" => Some(Self::Abstraction),
            "Perspective" | "perspective" => Some(Self::Perspective),
            _ => None,
        }
    }

    /// Map a stored memory kind. `Goal` is not a memory row.
    #[must_use]
    pub const fn from_entity_kind(kind: crate::EntityKind) -> Option<Self> {
        match kind {
            crate::EntityKind::Fact => Some(Self::Fact),
            crate::EntityKind::Abstraction => Some(Self::Abstraction),
            crate::EntityKind::Perspective => Some(Self::Perspective),
            crate::EntityKind::Goal => None,
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
