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

    /// The identity of the object store this adapter deletes from.
    ///
    /// The durable purge queue records the store each debt was incurred
    /// against, and the drain touches only rows naming this one: matching
    /// store to queue by boot convention alone means a deployment that was
    /// rewired deletes against the wrong store (deleting nothing, then
    /// clearing the debt), or silently stops reclaiming. Every store names
    /// itself; there is no wildcard. The built-in in-memory dev/test store
    /// is [`ObjectStoreId::memory`], so it cannot acknowledge an external
    /// store's debts when a host boots without that store's configuration.
    fn store_id(&self) -> &ObjectStoreId;
}

/// Longest stored object-store identity, in bytes.
pub const MAX_OBJECT_STORE_ID_BYTES: usize = 512;

/// Longest operator-chosen identity (`PROXIMA_S3_STORE_ID`), in bytes.
pub const MAX_OPERATOR_STORE_ID_BYTES: usize = 64;

/// Which object store holds a purge debt's bytes, or a staged upload's.
///
/// A parsed value, never free text, in one of three shapes:
///
/// - `s3:<origin>/<bucket>`, derived from an S3 endpoint URL and bucket by
///   [`Self::derive_s3`]: lower-case scheme and host, the default port
///   dropped, the path kept without a trailing `/`.
/// - `s3:aws/<bucket>` for the AWS endpoint resolution (no endpoint URL).
/// - `s3:id/<id>` when the operator names the store ([`Self::operator`]),
///   for processes that reach one store through different endpoint URLs, or
///   several stores behind one endpoint and bucket.
///
/// [`Self::memory`] is the in-process store; it can never equal an `s3:`
/// identity. The scheme prefix leaves room for other purge targets.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjectStoreId(String);

/// Why a value is not an [`ObjectStoreId`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ObjectStoreIdError {
    #[error("object store id must be 1 to {MAX_OPERATOR_STORE_ID_BYTES} of [A-Za-z0-9._-]")]
    InvalidOperatorId,
    #[error("object store endpoint URL is not an absolute URL: {0}")]
    InvalidEndpoint(String),
    #[error("object store bucket must be non-empty and contain no '/'")]
    InvalidBucket,
    #[error("stored object store id is malformed")]
    Malformed,
}

impl ObjectStoreId {
    /// The in-process store dev and test hosts wire without an object store.
    #[must_use]
    pub fn memory() -> Self {
        Self("memory:".into())
    }

    /// An operator-named S3 store: `s3:id/<id>`.
    ///
    /// # Errors
    /// [`ObjectStoreIdError::InvalidOperatorId`] unless `id` is 1 to 64 of
    /// `[A-Za-z0-9._-]`.
    pub fn operator(id: &str) -> Result<Self, ObjectStoreIdError> {
        if id.is_empty()
            || id.len() > MAX_OPERATOR_STORE_ID_BYTES
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            return Err(ObjectStoreIdError::InvalidOperatorId);
        }
        Ok(Self(format!("s3:id/{id}")))
    }

    /// Derive an S3 store's identity from its internal endpoint and bucket.
    ///
    /// # Errors
    /// [`ObjectStoreIdError::InvalidEndpoint`] when `endpoint_url` has no
    /// scheme or host; [`ObjectStoreIdError::InvalidBucket`] for an empty
    /// bucket or one containing `/`.
    pub fn derive_s3(endpoint_url: Option<&str>, bucket: &str) -> Result<Self, ObjectStoreIdError> {
        if bucket.is_empty() || bucket.contains('/') {
            return Err(ObjectStoreIdError::InvalidBucket);
        }
        let origin = match endpoint_url {
            None => "aws".to_owned(),
            Some(raw) => normalized_origin(raw)?,
        };
        let id = format!("s3:{origin}/{bucket}");
        if id.len() > MAX_OBJECT_STORE_ID_BYTES {
            return Err(ObjectStoreIdError::InvalidEndpoint(
                "endpoint URL is too long".into(),
            ));
        }
        Ok(Self(id))
    }

    /// Parse an identity read back from storage.
    ///
    /// # Errors
    /// [`ObjectStoreIdError::Malformed`] unless the value has a lower-case
    /// scheme prefix (`[a-z][a-z0-9]*:`) and fits the stored bound.
    pub fn parse(stored: &str) -> Result<Self, ObjectStoreIdError> {
        let Some((scheme, _)) = stored.split_once(':') else {
            return Err(ObjectStoreIdError::Malformed);
        };
        let mut bytes = scheme.bytes();
        let well_formed = stored.len() <= MAX_OBJECT_STORE_ID_BYTES
            && bytes.next().is_some_and(|byte| byte.is_ascii_lowercase())
            && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit());
        if !well_formed {
            return Err(ObjectStoreIdError::Malformed);
        }
        Ok(Self(stored.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ObjectStoreId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

fn normalized_origin(raw: &str) -> Result<String, ObjectStoreIdError> {
    let uri = raw
        .trim()
        .parse::<http::Uri>()
        .map_err(|error| ObjectStoreIdError::InvalidEndpoint(error.to_string()))?;
    let scheme = uri
        .scheme_str()
        .ok_or_else(|| ObjectStoreIdError::InvalidEndpoint("missing scheme".into()))?
        .to_ascii_lowercase();
    let host = uri
        .host()
        .ok_or_else(|| ObjectStoreIdError::InvalidEndpoint("missing host".into()))?
        .to_ascii_lowercase();
    let default_port = match scheme.as_str() {
        "http" => Some(80),
        "https" => Some(443),
        _ => None,
    };
    let port = uri
        .port_u16()
        .filter(|port| Some(*port) != default_port)
        .map(|port| format!(":{port}"))
        .unwrap_or_default();
    let path = uri.path().trim_end_matches('/');
    Ok(format!("{scheme}://{host}{port}{path}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// F17.1 (#488): a store that only purges is a whole store. Before S17
    /// this did not compile: the trait also demanded `put` and `get`.
    struct PurgeOnly(ObjectStoreId);

    #[async_trait::async_trait]
    impl ColdObjectStore for PurgeOnly {
        async fn delete(&self, _key: &str) -> Result<(), StorageError> {
            Ok(())
        }

        fn store_id(&self) -> &ObjectStoreId {
            &self.0
        }
    }

    fn s3(endpoint: Option<&str>, bucket: &str) -> String {
        ObjectStoreId::derive_s3(endpoint, bucket)
            .expect("valid S3 identity")
            .to_string()
    }

    #[test]
    fn a_purge_only_store_names_its_store() {
        let store = PurgeOnly(ObjectStoreId::operator("purge-only").expect("valid"));
        assert_eq!(store.store_id().as_str(), "s3:id/purge-only");
        assert_ne!(store.store_id(), &ObjectStoreId::memory());
    }

    #[test]
    fn derivation_normalizes_the_endpoint_origin() {
        assert_eq!(
            s3(Some("http://127.0.0.1:9000"), "b"),
            "s3:http://127.0.0.1:9000/b"
        );
        assert_eq!(
            s3(Some("HTTPS://S3.Example.COM:443/"), "b"),
            "s3:https://s3.example.com/b"
        );
        assert_eq!(
            s3(Some("http://minio.local:80"), "b"),
            "s3:http://minio.local/b"
        );
        assert_eq!(
            s3(Some("https://gw.example.com:8443/store/"), "b"),
            "s3:https://gw.example.com:8443/store/b"
        );
        assert_eq!(s3(None, "b"), "s3:aws/b");
        // Same bucket, another endpoint: another store.
        assert_ne!(
            s3(Some("http://one.local:9000"), "b"),
            s3(Some("http://two.local:9000"), "b")
        );
    }

    #[test]
    fn operator_ids_are_bounded_and_closed() {
        assert_eq!(
            ObjectStoreId::operator("tenant-7.store_a")
                .expect("valid")
                .as_str(),
            "s3:id/tenant-7.store_a"
        );
        for invalid in ["", "has space", "slash/inside", "colon:", &"x".repeat(65)] {
            assert_eq!(
                ObjectStoreId::operator(invalid),
                Err(ObjectStoreIdError::InvalidOperatorId),
                "{invalid:?}"
            );
        }
        assert!(ObjectStoreId::operator(&"x".repeat(64)).is_ok());
    }

    #[test]
    fn invalid_derivation_inputs_are_refused() {
        assert_eq!(
            ObjectStoreId::derive_s3(None, ""),
            Err(ObjectStoreIdError::InvalidBucket)
        );
        assert_eq!(
            ObjectStoreId::derive_s3(None, "a/b"),
            Err(ObjectStoreIdError::InvalidBucket)
        );
        assert!(matches!(
            ObjectStoreId::derive_s3(Some("not a url"), "b"),
            Err(ObjectStoreIdError::InvalidEndpoint(_))
        ));
        assert!(matches!(
            ObjectStoreId::derive_s3(Some("/relative"), "b"),
            Err(ObjectStoreIdError::InvalidEndpoint(_))
        ));
    }

    #[test]
    fn stored_ids_round_trip_and_reject_free_text() {
        for id in [
            ObjectStoreId::memory(),
            ObjectStoreId::operator("a").expect("valid"),
            ObjectStoreId::derive_s3(Some("http://h:1"), "b").expect("valid"),
        ] {
            assert_eq!(ObjectStoreId::parse(id.as_str()), Ok(id));
        }
        for malformed in ["", "bucket-name", "S3:aws/b", "1x:y", ":"] {
            assert_eq!(
                ObjectStoreId::parse(malformed),
                Err(ObjectStoreIdError::Malformed),
                "{malformed:?}"
            );
        }
    }
}
