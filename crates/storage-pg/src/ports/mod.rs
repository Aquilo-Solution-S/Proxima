use proxima_core::storage_ports::OwnerWritePermit;
use proxima_core::{OwnerId, StorageError};

mod embeddings;
mod goals;
mod host_state;
mod host_state_dispatch;
mod host_state_lifecycle;
mod ingest;
mod memory;
mod owner_inverse;
mod owners;
mod read;
mod write_session;

pub use host_state::PgHostStateParticipant;
pub use host_state_dispatch::{
    AgreedCommand, CommandRegistrationError, HostStateHandler, PgCommandDispatcher,
};
pub use host_state_lifecycle::PgHostStateLifecyclePort;
pub use write_session::{PgHostStateSession, SubjectMemberships};

fn validate_permit_owner(permit: &OwnerWritePermit, owner: &OwnerId) -> Result<(), StorageError> {
    if permit.owner() == owner {
        Ok(())
    } else {
        Err(StorageError::ConstraintViolation(
            "request owner does not match owner write permit".into(),
        ))
    }
}
