//! Bounded in-process reads that return bytes only after integrity verification.

use std::num::NonZeroU64;

use proxima_core::storage_ports::{
    CitedBlobIntegrityMismatch, CitedBlobReadError, CitedBlobReadPort, VerifiedCitedBlob,
};
use proxima_core::{AuthzContext, OwnerRef};
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;
use uuid::Uuid;

use super::CitedBlobStore;
use super::guards::ensure_owner_access;
use super::keys::locator_was_minted_here;
use super::rows::{BlobReadRecord, load_blob_read_record};

fn within_ceiling(byte_len: u64, max_bytes: NonZeroU64) -> Result<(), CitedBlobReadError> {
    if byte_len > max_bytes.get() {
        Err(CitedBlobReadError::TooLarge {
            byte_len,
            max_bytes: max_bytes.get(),
        })
    } else {
        Ok(())
    }
}

/// Byte-exact locator provenance, the same rule `read_url` applies.
///
/// The key derives from the row's own columns — its `upload_id`, or the
/// upload row it mounts — so a row can only ever vouch for the object it
/// was given, and never for one it names by assertion.
fn canonical_for_store(store: &CitedBlobStore, row: &BlobReadRecord) -> bool {
    row.bucket == store.config.bucket
        && locator_was_minted_here(&row.object_key, row.upload_id, row.mounted_from_upload_id)
}

async fn verify_body(
    body: aws_sdk_s3::primitives::ByteStream,
    row: &BlobReadRecord,
    max_bytes: NonZeroU64,
) -> Result<Vec<u8>, CitedBlobReadError> {
    within_ceiling(row.byte_len, max_bytes)?;
    let capacity = usize::try_from(row.byte_len).map_err(|_| CitedBlobReadError::TooLarge {
        byte_len: row.byte_len,
        max_bytes: max_bytes.get(),
    })?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(capacity)
        .map_err(|err| CitedBlobReadError::Unavailable(format!("reserve read buffer: {err}")))?;

    let mut reader = body.into_async_read();
    let mut blake3_hasher = blake3::Hasher::new();
    let mut sha256_hasher = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
    let mut byte_len = 0_u64;
    loop {
        // Read at most the remaining declared bytes plus one sentinel byte.
        // A forged/changed object therefore cannot make a small caller cap
        // consume a whole 64 KiB chunk before the mismatch is detected.
        let remaining = row.byte_len.saturating_sub(byte_len);
        let read_limit = usize::try_from(remaining.saturating_add(1))
            .unwrap_or(usize::MAX)
            .min(buffer.len());
        let read = reader
            .read(&mut buffer[..read_limit])
            .await
            .map_err(|err| CitedBlobReadError::Unavailable(format!("stream cited blob: {err}")))?;
        if read == 0 {
            break;
        }
        byte_len = byte_len
            .checked_add(u64::try_from(read).unwrap_or(u64::MAX))
            .ok_or(CitedBlobReadError::TooLarge {
                byte_len: u64::MAX,
                max_bytes: max_bytes.get(),
            })?;
        if byte_len > row.byte_len {
            return Err(CitedBlobReadError::IntegrityMismatch(
                CitedBlobIntegrityMismatch::ByteLength,
            ));
        }
        within_ceiling(byte_len, max_bytes)?;
        let chunk = &buffer[..read];
        blake3_hasher.update(chunk);
        sha256_hasher.update(chunk);
        bytes.extend_from_slice(chunk);
    }

    if byte_len != row.byte_len {
        return Err(CitedBlobReadError::IntegrityMismatch(
            CitedBlobIntegrityMismatch::ByteLength,
        ));
    }
    if blake3_hasher.finalize().as_bytes() != &row.content_hash {
        return Err(CitedBlobReadError::IntegrityMismatch(
            CitedBlobIntegrityMismatch::ContentHash,
        ));
    }
    let sha256: [u8; 32] = sha256_hasher.finalize().into();
    if sha256 != row.sha256 {
        return Err(CitedBlobReadError::IntegrityMismatch(
            CitedBlobIntegrityMismatch::Sha256,
        ));
    }
    Ok(bytes)
}

#[async_trait::async_trait]
impl CitedBlobReadPort for CitedBlobStore {
    async fn collect_verified(
        &self,
        authz: &AuthzContext,
        owner: OwnerRef,
        cited_object_id: Uuid,
        max_bytes: NonZeroU64,
    ) -> Result<VerifiedCitedBlob, CitedBlobReadError> {
        // This MUST precede even the owner-scoped row lookup: the row contains
        // the raw locator, so authorization after SQL would already be late.
        ensure_owner_access(authz, &owner).map_err(|_| CitedBlobReadError::AccessDenied)?;

        let row = load_blob_read_record(&self.pool, &owner, cited_object_id)
            .await
            .map_err(|err| CitedBlobReadError::Unavailable(err.to_string()))?
            .ok_or(CitedBlobReadError::NotFound)?;
        if !canonical_for_store(self, &row) {
            return Err(CitedBlobReadError::NotFound);
        }
        // Reject from immutable metadata before constructing the S3 client or
        // issuing GET. The stream repeats this bound against the actual bytes.
        within_ceiling(row.byte_len, max_bytes)?;

        let object =
            self.client()
                .await
                .map_err(|err| CitedBlobReadError::Unavailable(err.to_string()))?
                .get_object()
                .bucket(&row.bucket)
                .key(&row.object_key)
                .send()
                .await
                .map_err(|err| {
                    if err.as_service_error().is_some_and(
                        aws_sdk_s3::operation::get_object::GetObjectError::is_no_such_key,
                    ) {
                        CitedBlobReadError::NotFound
                    } else {
                        CitedBlobReadError::Unavailable(format!("read cited blob: {err}"))
                    }
                })?;
        if let Some(content_length) = object.content_length()
            && u64::try_from(content_length).ok() != Some(row.byte_len)
        {
            return Err(CitedBlobReadError::IntegrityMismatch(
                CitedBlobIntegrityMismatch::ByteLength,
            ));
        }
        let bytes = verify_body(object.body, &row, max_bytes).await?;

        Ok(VerifiedCitedBlob {
            cited_object_id: row.cited_object_id,
            content_hash: row.content_hash,
            sha256: row.sha256,
            byte_len: row.byte_len,
            mime: row.mime,
            filename: row.filename,
            bytes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::testkit::{lazy_test_pool, store_config};
    use super::*;
    use proxima_core::{AuthPath, UserId};

    fn record(bytes: &[u8]) -> BlobReadRecord {
        let sha256: [u8; 32] = Sha256::digest(bytes).into();
        BlobReadRecord {
            cited_object_id: Uuid::now_v7(),
            upload_id: Uuid::now_v7(),
            mounted_from_upload_id: None,
            content_hash: *blake3::hash(bytes).as_bytes(),
            bucket: "bucket".into(),
            object_key: "objects/owner/core/uploaded-blob-v1/hash".into(),
            sha256,
            byte_len: u64::try_from(bytes.len()).unwrap(),
            mime: "application/octet-stream".into(),
            filename: "blob.bin".into(),
        }
    }

    #[tokio::test]
    async fn denial_happens_before_locator_lookup() {
        let store =
            CitedBlobStore::new(lazy_test_pool(), store_config(None, None)).expect("test store");
        let allowed = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let denied = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let authz = AuthzContext::single_owner(&allowed, AuthPath::HostBearer);

        let error = store
            .collect_verified(&authz, denied, Uuid::now_v7(), NonZeroU64::new(1).unwrap())
            .await
            .expect_err("foreign owner denied");

        assert_eq!(error, CitedBlobReadError::AccessDenied);
    }

    #[tokio::test]
    async fn ceiling_rejects_from_metadata_before_streaming() {
        let row = record(b"four");
        let error = verify_body(
            aws_sdk_s3::primitives::ByteStream::from_static(b"four"),
            &row,
            NonZeroU64::new(3).unwrap(),
        )
        .await
        .expect_err("over-ceiling metadata rejected");

        assert_eq!(
            error,
            CitedBlobReadError::TooLarge {
                byte_len: 4,
                max_bytes: 3,
            }
        );
    }

    #[tokio::test]
    async fn truncated_body_returns_no_bytes() {
        let row = record(b"complete");
        let error = verify_body(
            aws_sdk_s3::primitives::ByteStream::from_static(b"short"),
            &row,
            NonZeroU64::new(64).unwrap(),
        )
        .await
        .expect_err("truncated body rejected");

        assert_eq!(
            error,
            CitedBlobReadError::IntegrityMismatch(CitedBlobIntegrityMismatch::ByteLength)
        );
    }

    #[tokio::test]
    async fn overlong_body_stops_at_the_first_extra_byte() {
        let row = record(b"four");
        let error = verify_body(
            aws_sdk_s3::primitives::ByteStream::from_static(b"five!"),
            &row,
            NonZeroU64::new(64).unwrap(),
        )
        .await
        .expect_err("overlong body rejected");

        assert_eq!(
            error,
            CitedBlobReadError::IntegrityMismatch(CitedBlobIntegrityMismatch::ByteLength)
        );
    }

    #[tokio::test]
    async fn digest_mismatch_returns_no_bytes() {
        let mut row = record(b"same-len");
        row.content_hash = [7; 32];
        let error = verify_body(
            aws_sdk_s3::primitives::ByteStream::from_static(b"same-len"),
            &row,
            NonZeroU64::new(64).unwrap(),
        )
        .await
        .expect_err("BLAKE3 mismatch rejected");
        assert_eq!(
            error,
            CitedBlobReadError::IntegrityMismatch(CitedBlobIntegrityMismatch::ContentHash)
        );

        let mut row = record(b"same-len");
        row.sha256 = [9; 32];
        let error = verify_body(
            aws_sdk_s3::primitives::ByteStream::from_static(b"same-len"),
            &row,
            NonZeroU64::new(64).unwrap(),
        )
        .await
        .expect_err("SHA-256 mismatch rejected");
        assert_eq!(
            error,
            CitedBlobReadError::IntegrityMismatch(CitedBlobIntegrityMismatch::Sha256)
        );
    }

    #[tokio::test]
    async fn valid_body_is_returned_only_after_all_checks() {
        let row = record(b"verified");
        let bytes = verify_body(
            aws_sdk_s3::primitives::ByteStream::from_static(b"verified"),
            &row,
            NonZeroU64::new(64).unwrap(),
        )
        .await
        .expect("valid body");

        assert_eq!(bytes, b"verified");
    }
}
