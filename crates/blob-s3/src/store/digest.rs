//! Stream the row-selected upload object once and derive everything the corpus needs
//! from that single pass: the content address, the audit digest, and the
//! true byte length.

use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;

use crate::error::BlobError;

/// How much this hasher is entitled to demand of the response it reads.
///
/// The distinction is diagnostic, not cosmetic. A declared length belongs to
/// a client's prepare and a mismatch is that client's error; an object this
/// store already published has no declaration to enforce, and its caller
/// compares digests afterwards. Reporting a length difference there as an
/// upload-length mismatch would misattribute a canonical conflict to the
/// client.
#[derive(Debug, Clone, Copy)]
pub(super) enum LengthExpectation {
    /// The upload row declared this length. Anything else is a mismatch, and
    /// the read stops one byte past the declaration.
    Declared(i64),
    /// Nothing to enforce: read bounded only by the configured cap.
    CapOnly,
}

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

impl StreamedObject {
    /// Move the buffered response out for publication. The digests and the
    /// length stay behind, which is everything a later comparison needs, so
    /// the bytes travel to the provider without a second full copy.
    pub(super) fn take_bytes(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.bytes)
    }
}

/// Reserve toward the declared length so the common case does not repeatedly
/// realloc, but never let a declaration size the first allocation on its own:
/// with no cap configured a forged `expected_byte_len` would otherwise ask the
/// allocator for that much up front. The in-loop length check bounds how far
/// the buffer can actually grow, so starting small costs a few reallocations
/// at worst.
const INITIAL_RESERVE_CEILING: usize = 8 * 1024 * 1024;

pub(super) async fn hash_uploaded_object(
    body: aws_sdk_s3::primitives::ByteStream,
    expected: LengthExpectation,
    max_blob_bytes: Option<u64>,
) -> Result<StreamedObject, BlobError> {
    let mut reader = body.into_async_read();
    let mut blake3_hasher = blake3::Hasher::new();
    let mut sha256_hasher = Sha256::new();
    let mut bytes = Vec::new();
    let declared = match expected {
        LengthExpectation::Declared(len) => Some(u64::try_from(len).unwrap_or(0)),
        LengthExpectation::CapOnly => None,
    };
    if let Some(declared) = declared {
        let capacity = usize::try_from(declared).unwrap_or(usize::MAX);
        let capacity = max_blob_bytes.map_or(capacity, |max| {
            capacity.min(usize::try_from(max).unwrap_or(usize::MAX))
        });
        bytes
            .try_reserve(capacity.min(INITIAL_RESERVE_CEILING))
            .map_err(|error| BlobError::State(format!("reserve upload buffer failed: {error}")))?;
    }
    let mut buf = vec![0_u8; 64 * 1024].into_boxed_slice();
    let mut byte_len = 0_u64;
    loop {
        // Read at most the remaining acceptable bytes plus one sentinel byte.
        // A forged or changed length therefore cannot make this bounded
        // verifier consume an unbounded response before rejecting it.
        let ceiling = match (declared, max_blob_bytes) {
            (Some(declared), Some(max)) => declared.min(max),
            (Some(declared), None) => declared,
            (None, Some(max)) => max,
            (None, None) => u64::MAX,
        };
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
        if let LengthExpectation::Declared(expected_byte_len) = expected
            && byte_len > declared.unwrap_or(0)
        {
            return Err(BlobError::State(format!(
                "uploaded byte length {byte_len} does not match expected {expected_byte_len}"
            )));
        }
        bytes.extend_from_slice(chunk);
    }
    if let LengthExpectation::Declared(expected_byte_len) = expected
        && i64::try_from(byte_len).unwrap_or(i64::MAX) != expected_byte_len
    {
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
            LengthExpectation::Declared(i64::try_from(input.len()).expect("test length fits")),
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
            LengthExpectation::Declared(3),
            Some(1024),
        )
        .await
        .expect_err("the first byte beyond the declaration must stop the read");

        assert_eq!(
            error.to_string(),
            "uploaded byte length 4 does not match expected 3"
        );
    }

    #[tokio::test]
    async fn cap_only_accepts_any_length_under_the_cap() {
        // A canonical object is read without a declaration precisely so a
        // length difference reaches the caller's digest comparison instead of
        // being reported as a client length error.
        let input: &'static [u8] = b"a canonical object of some other length";
        let streamed = hash_uploaded_object(
            aws_sdk_s3::primitives::ByteStream::from_static(input),
            LengthExpectation::CapOnly,
            Some(1024),
        )
        .await
        .expect("cap-only read succeeds");

        assert_eq!(streamed.byte_len, input.len() as u64);
        assert_eq!(streamed.bytes, input);
    }

    #[tokio::test]
    async fn cap_only_still_refuses_to_exceed_the_cap() {
        let error = hash_uploaded_object(
            aws_sdk_s3::primitives::ByteStream::from_static(b"larger than the cap"),
            LengthExpectation::CapOnly,
            Some(4),
        )
        .await
        .expect_err("the cap still bounds an undeclared read");

        assert_eq!(error.to_string(), "uploaded object exceeds max blob size 4");
    }

    #[tokio::test]
    async fn a_huge_declaration_does_not_preallocate_it() {
        // The declaration is attacker-influenced; the reservation must not be.
        let input: &'static [u8] = b"tiny";
        let error = hash_uploaded_object(
            aws_sdk_s3::primitives::ByteStream::from_static(input),
            LengthExpectation::Declared(i64::MAX),
            None,
        )
        .await
        .expect_err("a declaration far beyond the body is still a mismatch");

        assert!(
            error
                .to_string()
                .starts_with("uploaded byte length 4 does not match"),
            "expected a length mismatch, got {error}"
        );
    }
}
