//! Best-effort external object-store erase for owner-scope compliance.
//!
//! Core stays blob/storage-agnostic (docs/07): a compliance erase removes the
//! owner's authoritative Postgres rows in-band, but any external object store
//! holding the owner's cited-object payloads (e.g. an S3 bucket of uploaded OCR
//! documents) lives behind this host-wired port. The facade registers the
//! concrete blob backend; when the port is absent, owner erase behaves exactly
//! as before — Postgres rows only, no object-store call.

use crate::OwnerRef;
use crate::storage::StorageError;

/// Purge an owner's external cited-object payloads during an OWNER-scope
/// compliance erase.
///
/// Best-effort: the authoritative Postgres rows are already committed-deleted
/// by the time this runs, and the object store is eventually consistent, so a
/// failed or partial purge must never resurrect deleted rows nor fail the
/// erase. The engine logs and swallows errors from this port.
#[async_trait::async_trait]
pub trait CitedObjectErasePort: Send + Sync {
    /// Delete every stored object owned by `owner`, returning the count of
    /// objects deleted.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when the object store cannot be listed or the
    /// deletes fail; the caller treats this as non-fatal.
    async fn purge_owner_objects(&self, owner: OwnerRef) -> Result<u64, StorageError>;
}
