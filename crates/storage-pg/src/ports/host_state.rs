//! Startup-registered host-state participant for [`super::write_session`].

use proxima_core::StorageError;
use proxima_core::storage_ports::{HostStateReply, HostStateRequest, OwnerWritePermit};
use sqlx::{Postgres, Transaction};

/// Trusted host/backend code that mutates declared state surfaces on the
/// write session's live transaction.
///
/// Registered once at boot on [`crate::PgStorage`]. Flavor tools never
/// receive this trait, the transaction, or a generic SQL hole.
#[async_trait::async_trait]
pub trait PgHostStateParticipant: Send + Sync {
    /// Stable id matched against [`proxima_core::storage_ports::HostStateCommand::PARTICIPANT_ID`].
    fn participant_id(&self) -> &'static str;

    /// Tables this participant is allowed to touch. Must be a subset of
    /// some linked flavor's `state_surfaces`.
    fn declared_tables(&self) -> &'static [&'static str];

    /// Run `request` on `tx`. The session has already checked participant
    /// id and declared tables; SQL must stamp the permit's owner.
    ///
    /// # Errors
    ///
    /// Storage faults from the host-owned statements. Returning `Err` after
    /// SQL has succeeded is a real failure: the unit of work poisons and
    /// will not commit.
    async fn apply(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        permit: &OwnerWritePermit,
        request: HostStateRequest,
    ) -> Result<HostStateReply, StorageError>;
}
