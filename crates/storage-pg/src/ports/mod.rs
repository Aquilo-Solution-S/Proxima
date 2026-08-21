use proxima_core::storage_ports::OwnerWritePermit;
use proxima_core::{Owner, StorageError};

mod embeddings;
mod goals;
mod ingest;
mod memory;
mod owner_inverse;
mod owners;
mod read;
mod registry;
mod write_session;

fn validate_permit_owner(permit: &OwnerWritePermit, owner: &Owner) -> Result<(), StorageError> {
    if permit.owner() == owner {
        Ok(())
    } else {
        Err(StorageError::ConstraintViolation(
            "request owner does not match owner write permit".into(),
        ))
    }
}
