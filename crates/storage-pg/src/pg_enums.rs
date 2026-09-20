//! Typed decodes for the closed `proxima_core` Postgres enums.
//!
//! A `kind::text` readback matched on `&str` has no total arm. The catch-all
//! must pick some value, and that pick is a guess the compiler cannot check —
//! which is how one enum grew four different unknown-label behaviours across
//! ten decode sites, three of them silently coercing to the most destructive
//! reading (`forget` treating an unknown kind as a Fact, `announce` reporting
//! a deletion as an append).
//!
//! Decoding into a `sqlx::Type` enum moves that decision to the boundary: an
//! unrecognised label is a decode error where the row is read, and adding a
//! Rust variant is a compile error in the `From` impls below rather than a
//! silent re-route through a catch-all.

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

/// `proxima_core.announce_op`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "proxima_core.announce_op", rename_all = "lowercase")]
pub(crate) enum PgAnnounceOp {
    Append,
    Forget,
    Erase,
    Transfer,
}

/// `proxima_core.announce_entity`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "proxima_core.announce_entity", rename_all = "lowercase")]
pub(crate) enum PgAnnounceEntity {
    Memory,
    Goal,
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
