//! The completed upload a blob's ordinary reads resolve.

// Both aliases are part of this private query fragment: `b` is the blob
// being resolved and `u` is its newest completed, owner-matching upload.
// Keep the stored upload id and mounted provenance intact; callers apply
// the same locator gate to the resulting coordinates.
macro_rules! with_live_blob_upload {
    ($prefix:literal, $suffix:literal) => {
        concat!(
            $prefix,
            " LATERAL (
                SELECT candidate.*
                  FROM proxima_core.blob_uploads candidate
                 WHERE candidate.blob_id = b.blob_id
                   AND candidate.owner_id = b.owner_id
                   AND candidate.status = 'completed'
                 ORDER BY candidate.completed_at DESC NULLS LAST,
                          candidate.upload_id DESC
                 LIMIT 1
              ) u ON true ",
            $suffix
        )
    };
}

pub(super) use with_live_blob_upload;
