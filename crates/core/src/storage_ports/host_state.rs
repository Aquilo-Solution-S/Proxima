//! Typed host-state participation inside a [`super::WriteSession`].
//!
//! Core stays backend-neutral: a command is an opaque payload plus the
//! tables it will touch. The Postgres session dispatches that payload to
//! a startup-registered participant on the **same** transaction it already
//! holds. The participant never escapes the transaction, and Flavor SDK /
//! tools never receive SQL, a pool, or a generic administration surface.

use std::any::Any;
use std::fmt;

use crate::Owner;
use crate::storage::StorageError;

/// Typed host-owned command executed inside a [`crate::engine::UnitOfWork`].
///
/// `TABLES` must be declared on some linked flavor's
/// [`crate::FlavorContract::state_surfaces`]. The engine refuses the command
/// before any mutation when they are not. `PARTICIPANT_ID` must match the
/// participant registered on the Postgres write session at boot.
pub trait HostStateCommand: Send + 'static {
    /// Stable id of the startup-registered participant that understands
    /// this command.
    const PARTICIPANT_ID: &'static str;
    /// Physical tables this command will read or write. Each name is a
    /// declared state surface, not a memory sidecar and not a kernel table.
    const TABLES: &'static [&'static str];
    /// Typed business result carried by [`HostStateOutcome`].
    type Outcome: Send + 'static;
    /// Destination owner. Authorized through the existing write gate
    /// before dispatch; SQL stamps this owner from the resulting permit,
    /// never from a caller-supplied column.
    fn owner(&self) -> Owner;
}

/// Result of a host-state command that ran (or was refused) without a
/// storage fault.
///
/// `AlreadyApplied` and `Refused` are successful unit-of-work answers:
/// they write no duplicate business rows. A participant error is a
/// [`StorageError`] and poisons the unit so it cannot be committed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostStateOutcome<T> {
    /// The transition was applied in this unit.
    Permitted(T),
    /// The transition was already the stored state; nothing was duplicated.
    AlreadyApplied(T),
    /// The transition is not legal from the stored state; nothing was written.
    Refused(T),
}

impl<T> HostStateOutcome<T> {
    /// Whether this outcome applied a new transition.
    #[must_use]
    pub const fn is_permitted(&self) -> bool {
        matches!(self, Self::Permitted(_))
    }

    /// Whether the stored state already matched the requested transition.
    #[must_use]
    pub const fn is_already_applied(&self) -> bool {
        matches!(self, Self::AlreadyApplied(_))
    }

    /// Whether the stored state refused the requested transition.
    #[must_use]
    pub const fn is_refused(&self) -> bool {
        matches!(self, Self::Refused(_))
    }
}

/// Opaque command envelope handed to the write session.
///
/// Built only from a [`HostStateCommand`]. Core does not interpret `payload`.
pub struct HostStateRequest {
    participant_id: &'static str,
    tables: &'static [&'static str],
    payload: Box<dyn Any + Send>,
}

impl fmt::Debug for HostStateRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HostStateRequest")
            .field("participant_id", &self.participant_id)
            .field("tables", &self.tables)
            .finish_non_exhaustive()
    }
}

impl HostStateRequest {
    /// Box `command` for dispatch. The session matches `PARTICIPANT_ID`
    /// and `TABLES` against the registered participant and declared
    /// state surfaces.
    #[must_use]
    pub fn from_command<C: HostStateCommand>(command: C) -> Self {
        Self {
            participant_id: C::PARTICIPANT_ID,
            tables: C::TABLES,
            payload: Box::new(command),
        }
    }

    /// Participant id declared by the command type.
    #[must_use]
    pub const fn participant_id(&self) -> &'static str {
        self.participant_id
    }

    /// Tables the command declared it will touch.
    #[must_use]
    pub const fn tables(&self) -> &'static [&'static str] {
        self.tables
    }

    /// Recover the typed command. A mismatch is a participant programming
    /// error, not a caller-fixable argument.
    ///
    /// # Errors
    ///
    /// `Internal` when the boxed type is not `C`.
    pub fn downcast<C: HostStateCommand>(self) -> Result<C, StorageError> {
        let Self {
            participant_id,
            payload,
            ..
        } = self;
        payload.downcast::<C>().map(|boxed| *boxed).map_err(|_| {
            StorageError::Internal(format!(
                "host-state command type mismatch for participant {participant_id}"
            ))
        })
    }
}

/// Classification of a host-state reply. Mirrors [`HostStateOutcome`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostStateReplyKind {
    /// The transition was applied in this unit.
    Permitted,
    /// The stored state already matched; nothing was duplicated.
    AlreadyApplied,
    /// The stored state refused the transition; nothing was written.
    Refused,
}

/// Opaque typed reply from a host-state participant.
pub struct HostStateReply {
    kind: HostStateReplyKind,
    payload: Box<dyn Any + Send>,
}

impl fmt::Debug for HostStateReply {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HostStateReply")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

impl HostStateReply {
    /// A newly applied transition.
    #[must_use]
    pub fn permitted<T: Send + 'static>(value: T) -> Self {
        Self {
            kind: HostStateReplyKind::Permitted,
            payload: Box::new(value),
        }
    }

    /// The stored state already matched the requested transition.
    #[must_use]
    pub fn already_applied<T: Send + 'static>(value: T) -> Self {
        Self {
            kind: HostStateReplyKind::AlreadyApplied,
            payload: Box::new(value),
        }
    }

    /// The stored state refused the requested transition.
    #[must_use]
    pub fn refused<T: Send + 'static>(value: T) -> Self {
        Self {
            kind: HostStateReplyKind::Refused,
            payload: Box::new(value),
        }
    }

    /// Recover the typed result and its classification.
    ///
    /// # Errors
    ///
    /// `Internal` when the boxed type is not `T`.
    pub fn downcast<T: Send + 'static>(self) -> Result<(HostStateReplyKind, T), StorageError> {
        self.payload
            .downcast::<T>()
            .map(|boxed| (self.kind, *boxed))
            .map_err(|_| {
                StorageError::Internal(
                    "host-state participant returned an unexpected result type".into(),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::{HostStateCommand, HostStateReply, HostStateReplyKind, HostStateRequest};
    use crate::{GroupId, OwnerRef};
    use uuid::Uuid;

    struct Dummy {
        owner: crate::Owner,
    }

    impl HostStateCommand for Dummy {
        const PARTICIPANT_ID: &'static str = "dummy";
        const TABLES: &'static [&'static str] = &["dummy.table"];
        type Outcome = u8;

        fn owner(&self) -> crate::Owner {
            self.owner
        }
    }

    #[test]
    fn request_round_trips_the_command() {
        let owner = OwnerRef::Group(GroupId::new(Uuid::nil()));
        let request = HostStateRequest::from_command(Dummy { owner });
        assert_eq!(request.participant_id(), "dummy");
        assert_eq!(request.tables(), &["dummy.table"]);
        let command = request.downcast::<Dummy>().expect("command type");
        assert_eq!(command.owner(), owner);
    }

    #[test]
    fn reply_round_trips_permitted() {
        let reply = HostStateReply::permitted(7_u8);
        let (kind, value) = reply.downcast::<u8>().expect("result type");
        assert_eq!(kind, HostStateReplyKind::Permitted);
        assert_eq!(value, 7);
    }
}
