//! `core/upload-v1` — the Fact that a file entered the corpus.
//!
//! The upload lane stored the artefact and stopped there: a
//! `core/uploaded-blob-v1` cited object, content-addressed by BLAKE3, and
//! its id handed back. What nothing wrote was a record that the upload
//! HAPPENED. A cited object is a thing that can be cited; it is not an
//! event, it carries no receipt, it appears in no change history, and
//! nothing in the substrate says who put it there or when. Flavors filled
//! that in themselves, which made "a file entered the corpus" a per-flavor
//! guarantee — true in the flavors that bothered, absent everywhere else.
//!
//! This schema makes it a substrate guarantee. The Fact cites the artefact
//! through `core/uploaded-blob-whole-v1`, so the event and the bytes are one
//! hop apart in both directions — and, since the citation is what persists
//! the artefact, they are written together or not at all
//! ([`crate::engine::UploadCompleted`]).

use crate::{FactPayload, PayloadKeyBuilder};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// One file's arrival, as the store recorded it.
///
/// Every field here is a copy of what the completed upload reported, and
/// the upload reports what it STORED — on a replay of the same bytes under
/// a different name, that is the first upload's filename, not the second
/// caller's. The Fact therefore describes the artefact in the corpus, which
/// is the thing a reader can go and fetch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UploadV1 {
    /// Filename under which the artefact is held, as reported by the store.
    pub filename: String,
    /// MIME type declared at prepare time.
    pub mime: String,
    /// Exact byte length of the stored artefact.
    pub byte_len: u64,
    /// Lowercase-hex BLAKE3 of the bytes: the artefact's identity, the
    /// upload lane's dedup key, and this Fact's replay key all at once.
    pub content_hash: String,
}

impl FactPayload for UploadV1 {
    const SCHEMA_ID: &'static str = "core/upload-v1";
    const SCHEMA_VERSION: u32 = 1;

    /// The content hash alone.
    ///
    /// The other three fields are FUNCTIONS of it: for one owner, the
    /// store resolves a content hash to exactly one cited object, and
    /// reports that object's filename, mime, and length. Keying on them
    /// too would add no discrimination while creating a way for one file
    /// to acquire two upload Facts if that derivation ever shifted. One
    /// file, one upload Fact, per owner — the same identity the blob lane
    /// already dedups on, so the Fact replays exactly when the upload
    /// does.
    fn receipt_key(&self) -> Vec<u8> {
        let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
        key.field_str("content_hash", &self.content_hash);
        key.finish()
    }

    fn render(&self) -> String {
        format!(
            "uploaded {}\n{}, {} bytes",
            self.filename, self.mime, self.byte_len
        )
    }

    // `sidecar_table` stays at its `None` default. The typed description of
    // the artefact is already stored, once, in
    // `proxima_core.cited_uploaded_blob_v1`, and the citation this Fact
    // carries is how a reader reaches it. A sidecar here would be a second
    // copy of that row under a second identity, and the two would disagree
    // the first time one of them was corrected.
}

#[cfg(test)]
mod tests {
    use super::*;

    fn upload(filename: &str, mime: &str, byte_len: u64, content_hash: &str) -> UploadV1 {
        UploadV1 {
            filename: filename.into(),
            mime: mime.into(),
            byte_len,
            content_hash: content_hash.into(),
        }
    }

    /// The replay key is the file, not the description of the file. Two
    /// completions of the same bytes must land on one Fact even if the
    /// surrounding metadata is read back differently.
    #[test]
    fn the_same_content_hash_is_the_same_receipt_key() {
        let first = upload("handbuch.pdf", "application/pdf", 2048, &"ab".repeat(32));
        let renamed = upload(
            "kopie.pdf",
            "application/octet-stream",
            4096,
            &"ab".repeat(32),
        );

        assert_eq!(first.receipt_key(), renamed.receipt_key());
    }

    #[test]
    fn a_different_content_hash_is_a_different_receipt_key() {
        let first = upload("handbuch.pdf", "application/pdf", 2048, &"ab".repeat(32));
        let other = upload("handbuch.pdf", "application/pdf", 2048, &"cd".repeat(32));

        assert_ne!(first.receipt_key(), other.receipt_key());
    }

    /// The filename is the searchable part of an upload — it is the only
    /// thing a person knows about a file they are looking for.
    #[test]
    fn the_render_carries_the_filename() {
        let rendered = upload("Fassaden Atlas.pdf", "application/pdf", 2048, "ab").render();

        assert!(
            rendered.contains("Fassaden Atlas.pdf"),
            "the filename must reach the indexed text: {rendered}"
        );
        assert!(rendered.contains("application/pdf"), "{rendered}");
    }

    /// A Fact with no sidecar table of its own is the point of this
    /// schema, not an oversight: storage skips sidecar registration
    /// entirely for such schemas, so a table declared here would demand a
    /// migration and a second copy of the cited object.
    #[test]
    fn the_upload_fact_declares_no_sidecar_table() {
        assert_eq!(UploadV1::sidecar_table(), None);
    }
}
