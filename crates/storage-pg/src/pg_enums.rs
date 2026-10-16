//! Typed decodes for the closed `proxima_core` Postgres enums.
//!
//! Database labels are decoded at the row boundary. Unknown labels return a
//! decode error; exhaustive `From` matches cover every admitted Rust variant.

use proxima_core::EntityKind;

/// `proxima_core.memory_kind`. Three labels — `Goal` is not one of them; a
/// goal is a separate entity, not a memory kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "proxima_core.memory_kind", rename_all = "lowercase")]
pub(crate) enum PgMemoryKind {
    Fact,
    Abstraction,
    Perspective,
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
