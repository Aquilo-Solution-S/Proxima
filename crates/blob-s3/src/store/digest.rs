//! Stream the row-selected upload object once and derive everything the corpus needs
//! from that single pass: the content address, the audit digest, and the
//! true byte length.

use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;

use crate::error::BlobError;

#[derive(Debug, Clone)]
pub(super) struct StreamedObject {
    pub(super) blake3: [u8; 32],
    pub(super) sha256: [u8; 32],
    pub(super) byte_len: u64,
    /// The exact bounded response that produced both digests. Keeping this
    /// buffer avoids a second GET racing a client overwrite between hashing
    /// and canonical publication.
    pub(super) bytes: Vec<u8>,
}

pub(super) async fn hash_uploaded_object(
    body: aws_sdk_s3::primitives::ByteStream,
    expected_byte_len: i64,
    max_blob_bytes: Option<u64>,
) -> Result<StreamedObject, BlobError> {
    let mut reader = body.into_async_read();
    let mut blake3_hasher = blake3::Hasher::new();
    let mut sha256_hasher = Sha256::new();
    let mut bytes = Vec::new();
    let declared = u64::try_from(expected_byte_len).unwrap_or(0);
    if expected_byte_len >= 0 {
        let capacity = usize::try_from(expected_byte_len).unwrap_or(0);
        let capacity = max_blob_bytes.map_or(capacity, |max| {
            capacity.min(usize::try_from(max).unwrap_or(usize::MAX))
        });
        bytes
            .try_reserve(capacity)
            .map_err(|error| BlobError::State(format!("reserve upload buffer failed: {error}")))?;
    }
    let mut buf = vec![0_u8; 64 * 1024].into_boxed_slice();
    let mut byte_len = 0_u64;
    loop {
        // Read at most the remaining declared bytes plus one sentinel byte.
        // A forged or changed length therefore cannot make this bounded
        // verifier consume an unbounded response before rejecting it.
        let ceiling = max_blob_bytes.map_or(declared, |max| declared.min(max));
        let read_limit = usize::try_from(ceiling.saturating_sub(byte_len).saturating_add(1))
            .unwrap_or(usize::MAX)
            .min(buf.len());
        let n = reader
            .read(&mut buf[..read_limit])
            .await
            .map_err(|e| BlobError::S3(format!("stream pending upload failed: {e}")))?;
        if n == 0 {
            break;
        }
        let chunk = &buf[..n];
        blake3_hasher.update(chunk);
        sha256_hasher.update(chunk);
        byte_len = byte_len
            .checked_add(u64::try_from(n).unwrap_or(u64::MAX))
            .ok_or_else(|| BlobError::State("uploaded object is too large".into()))?;
        // Abort as soon as the streamed length crosses the cap so a client that
        // under-declared `byte_len` cannot force us to buffer/hash an oversized
        // object.
        if let Some(max) = max_blob_bytes
            && byte_len > max
        {
            return Err(BlobError::State(format!(
                "uploaded object exceeds max blob size {max}"
            )));
        }
        if byte_len > declared {
            return Err(BlobError::State(format!(
                "uploaded byte length {byte_len} does not match expected {expected_byte_len}"
            )));
        }
        bytes.extend_from_slice(chunk);
    }
    if i64::try_from(byte_len).unwrap_or(i64::MAX) != expected_byte_len {
        return Err(BlobError::State(format!(
            "uploaded byte length {byte_len} does not match expected {expected_byte_len}"
        )));
    }
    let blake3 = *blake3_hasher.finalize().as_bytes();
    let sha256: [u8; 32] = sha256_hasher.finalize().into();
    Ok(StreamedObject {
        blake3,
        sha256,
        byte_len,
        bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn one_response_supplies_bytes_and_both_digests() {
        let input: &'static [u8] = b"one bounded response";
        let streamed = hash_uploaded_object(
            aws_sdk_s3::primitives::ByteStream::from_static(input),
            i64::try_from(input.len()).expect("test input length fits"),
            Some(1024),
        )
        .await
        .expect("hash response");

        assert_eq!(streamed.bytes, input);
        assert_eq!(streamed.byte_len, input.len() as u64);
        assert_eq!(streamed.blake3, *blake3::hash(input).as_bytes());
        let expected_sha256: [u8; 32] = Sha256::digest(input).into();
        assert_eq!(streamed.sha256, expected_sha256);
    }

    #[tokio::test]
    async fn overlong_response_stops_after_one_sentinel_byte() {
        let error = hash_uploaded_object(
            aws_sdk_s3::primitives::ByteStream::from_static(b"far too long"),
            3,
            Some(1024),
        )
        .await
        .expect_err("the first byte beyond the declaration must stop the read");

        assert_eq!(
            error.to_string(),
            "uploaded byte length 4 does not match expected 3"
        );
    }
}
