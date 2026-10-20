//! Typed decodes for the closed `proxima_core` Postgres enums.
//!
//! Database labels are decoded at the row boundary. Unknown labels return a
//! decode error; exhaustive `From` matches cover every admitted Rust variant.

use proxima_core::verbs::schema::PayloadKind;
use proxima_core::{EntityKind, MemoryKind, StorageError};

/// `proxima_core.memory_kind`. Three labels — `Goal` is not one of them; a
/// goal is a separate entity, not a memory kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "proxima_core.memory_kind", rename_all = "lowercase")]
pub(crate) enum PgMemoryKind {
    Fact,
    Abstraction,
    Perspective,
}

impl PgMemoryKind {
    /// The database label: what a statement spells and a message names.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Fact => "fact",
            Self::Abstraction => "abstraction",
            Self::Perspective => "perspective",
        }
    }

    /// The labels of `kinds`, to bind as a `text[]` filter.
    pub(crate) fn labels(kinds: &[Self]) -> Vec<&'static str> {
        kinds.iter().map(|kind| kind.label()).collect()
    }

    /// The memory kind of an entity kind; a Goal is not a memory.
    pub(crate) const fn of(kind: EntityKind) -> Option<Self> {
        match kind {
            EntityKind::Fact => Some(Self::Fact),
            EntityKind::Abstraction => Some(Self::Abstraction),
            EntityKind::Perspective => Some(Self::Perspective),
            EntityKind::Goal => None,
        }
    }
}

impl From<MemoryKind> for PgMemoryKind {
    fn from(value: MemoryKind) -> Self {
        match value {
            MemoryKind::Fact => Self::Fact,
            MemoryKind::Abstraction => Self::Abstraction,
            MemoryKind::Perspective => Self::Perspective,
        }
    }
}

impl From<PgMemoryKind> for MemoryKind {
    fn from(value: PgMemoryKind) -> Self {
        match value {
            PgMemoryKind::Fact => Self::Fact,
            PgMemoryKind::Abstraction => Self::Abstraction,
            PgMemoryKind::Perspective => Self::Perspective,
        }
    }
}

impl From<PgMemoryKind> for EntityKind {
    fn from(value: PgMemoryKind) -> Self {
        match value {
            PgMemoryKind::Fact => Self::Fact,
            PgMemoryKind::Abstraction => Self::Abstraction,
            PgMemoryKind::Perspective => Self::Perspective,
        }
    }
}

impl From<PgMemoryKind> for PayloadKind {
    fn from(value: PgMemoryKind) -> Self {
        match value {
            PgMemoryKind::Fact => Self::Fact,
            PgMemoryKind::Abstraction => Self::Abstraction,
            PgMemoryKind::Perspective => Self::Perspective,
        }
    }
}

/// The payload layer a schema spec's entity kind writes. A spec names an
/// `EntityKind`, so a Goal is representable there and refused here.
pub(crate) fn memory_payload_kind(kind: EntityKind) -> Result<PayloadKind, StorageError> {
    PgMemoryKind::of(kind)
        .map(PayloadKind::from)
        .ok_or_else(|| StorageError::Internal("a Goal has no memory payload kind".into()))
}

/// `proxima_core.sketch_kind`: the memory kinds plus `goal`, the kinds a
/// sketch row can describe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "proxima_core.sketch_kind", rename_all = "lowercase")]
pub(crate) enum PgSketchKind {
    Fact,
    Abstraction,
    Perspective,
    Goal,
}

impl PgSketchKind {
    /// The database label, which is also the line a sketch falls back to.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Fact => "fact",
            Self::Abstraction => "abstraction",
            Self::Perspective => "perspective",
            Self::Goal => "goal",
        }
    }
}

impl From<EntityKind> for PgSketchKind {
    fn from(value: EntityKind) -> Self {
        match value {
            EntityKind::Fact => Self::Fact,
            EntityKind::Abstraction => Self::Abstraction,
            EntityKind::Perspective => Self::Perspective,
            EntityKind::Goal => Self::Goal,
        }
    }
}

impl From<PgMemoryKind> for PgSketchKind {
    fn from(value: PgMemoryKind) -> Self {
        match value {
            PgMemoryKind::Fact => Self::Fact,
            PgMemoryKind::Abstraction => Self::Abstraction,
            PgMemoryKind::Perspective => Self::Perspective,
        }
    }
}

impl From<PgSketchKind> for EntityKind {
    fn from(value: PgSketchKind) -> Self {
        match value {
            PgSketchKind::Fact => Self::Fact,
            PgSketchKind::Abstraction => Self::Abstraction,
            PgSketchKind::Perspective => Self::Perspective,
            PgSketchKind::Goal => Self::Goal,
        }
    }
}

/// `proxima_core.pin_target_kind`. Four labels: the same three as
/// [`PgMemoryKind`] plus `goal`, because a pin target may be a Goal.
///
/// This is the WIDER of the two lifecycle vocabularies, so a query that
/// coalesces a memory kind together with a pin-target kind unifies on this
/// one. Postgres has no direct enum-to-enum cast, so that widening spells
/// `::text::proxima_core.pin_target_kind` in SQL — a cast between two closed
/// types, not the `kind::text` readback into a Rust `String` this module
/// exists to remove.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "proxima_core.pin_target_kind", rename_all = "lowercase")]
pub(crate) enum PgPinTargetKind {
    Fact,
    Abstraction,
    Perspective,
    Goal,
}

impl From<PgPinTargetKind> for EntityKind {
    fn from(value: PgPinTargetKind) -> Self {
        match value {
            PgPinTargetKind::Fact => Self::Fact,
            PgPinTargetKind::Abstraction => Self::Abstraction,
            PgPinTargetKind::Perspective => Self::Perspective,
            PgPinTargetKind::Goal => Self::Goal,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_goal_has_no_memory_payload_kind() {
        assert!(matches!(
            memory_payload_kind(EntityKind::Fact),
            Ok(PayloadKind::Fact)
        ));
        assert!(matches!(
            memory_payload_kind(EntityKind::Perspective),
            Ok(PayloadKind::Perspective)
        ));
        assert!(matches!(
            memory_payload_kind(EntityKind::Goal),
            Err(StorageError::Internal(_))
        ));
    }

    #[test]
    fn a_goal_is_a_sketch_kind_but_not_a_memory_kind() {
        assert_eq!(PgMemoryKind::of(EntityKind::Goal), None);
        assert_eq!(PgSketchKind::from(EntityKind::Goal).label(), "goal");
    }
}
