//! Three-state owner mode for a memory sidecar.

/// How a memory sidecar relates to its memory's owner.
///
/// - [`None`](Self::None): no `owner_id` column; the row reaches its owner
///   through the memory (`StaysOnKey` / `Follow` without an own column).
/// - [`Pinned`](Self::Pinned): stamps `owner_id` from the memory at insert and
///   keeps it (`TransferRule::RetainAtSource`); forget/erase/export skip it and hydrate with
///   the owner-join select.
/// - [`Follows`](Self::Follows): stamps `owner_id` from the memory at insert
///   (same SQL as Pinned) and keeps it equal to the memory's owner: transfer
///   rehomes it (`TransferRule::Follow` + `Surface.owner_column`);
///   forget/erase/export walk it with the memory, and hydrate uses the
///   ordinary batch select. Its RLS policies still read through
///   `proxima_core.memory` (`install_owner_rls`'s `memory_owner_tables`), so
///   the row is visible exactly when its memory is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidecarOwnerMode {
    None,
    Pinned,
    Follows,
}

impl SidecarOwnerMode {
    /// Whether the sidecar table has an `owner_id` column.
    #[must_use]
    pub const fn has_owner_column(self) -> bool {
        matches!(self, Self::Pinned | Self::Follows)
    }

    /// Whether forget/erase/export skip this table and hydrate uses the
    /// pinned owner-join select.
    #[must_use]
    pub const fn is_pinned(self) -> bool {
        matches!(self, Self::Pinned)
    }

    /// Whether insert stamps `owner_id` from `proxima_core.memory`.
    #[must_use]
    pub const fn stamps_owner_at_insert(self) -> bool {
        self.has_owner_column()
    }
}
