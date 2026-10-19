//! The object store a purge deletes from.
//!
//! Forget keeps the Memory row (#488), so nothing is written to or read from
//! an object store on forget or restore. What is left is the durable purge
//! queue's other end: deleting an object a hard erase let go of.

use crate::StorageError;

/// Deletes objects the durable purge queue names.
#[async_trait::async_trait]
pub trait ColdObjectStore: Send + Sync {
    async fn delete(&self, key: &str) -> Result<(), StorageError>;

    /// This store's backend identity — the S3 bucket for the S3 adapter.
    ///
    /// The durable purge queue records the backend each debt was incurred
    /// against, and the drain refuses a row whose backend is not this one:
    /// matching store to queue by boot convention alone means a deployment
    /// that changed buckets deletes against the wrong store, or silently
    /// stops reclaiming. Every store names its backend; there is no
    /// wildcard. The built-in in-memory dev/test store names its own, so it
    /// cannot acknowledge an external store's debts when a host boots
    /// without that store's configuration.
    fn backend(&self) -> &str;
}

/// Whether the store wired now may drain a queue row enqueued against
/// `row_backend`.
#[must_use]
pub fn cold_backend_matches(store_backend: &str, row_backend: &str) -> bool {
    row_backend == store_backend
}

#[cfg(test)]
mod tests {
    use super::*;

    /// F17.1 (#488): a store that only purges is a whole store. Before S17
    /// this did not compile: the trait also demanded `put` and `get`.
    struct PurgeOnly;

    #[async_trait::async_trait]
    impl ColdObjectStore for PurgeOnly {
        async fn delete(&self, _key: &str) -> Result<(), StorageError> {
            Ok(())
        }

        fn backend(&self) -> &'static str {
            "purge-only"
        }
    }

    #[test]
    fn a_purge_only_store_matches_only_its_own_backend() {
        let store = PurgeOnly;
        assert!(cold_backend_matches(store.backend(), "purge-only"));
        assert!(!cold_backend_matches(store.backend(), ""));
        assert!(!cold_backend_matches("", "purge-only"));
    }
}
