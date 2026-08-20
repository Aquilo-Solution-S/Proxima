//! Stream a pending object once and derive everything the corpus needs
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
}

pub(super) async fn hash_uploaded_object(
    body: aws_sdk_s3::primitives::ByteStream,
    expected_byte_len: i64,
    max_blob_bytes: Option<u64>,
) -> Result<StreamedObject, BlobError> {
    let mut reader = body.into_async_read();
    let mut blake3_hasher = blake3::Hasher::new();
    let mut sha256_hasher = Sha256::new();
    let mut buf = vec![0_u8; 64 * 1024].into_boxed_slice();
    let mut byte_len = 0_u64;
    loop {
        let n = reader
            .read(&mut buf)
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
    })
}
