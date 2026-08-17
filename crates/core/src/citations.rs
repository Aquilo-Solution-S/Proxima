//! Core cited-object schemas, and the mappings that point Facts at them.
//!
//! A Fact reaches a cited object through `memory.blob_id` (0..1).
//! `authorize_fact_with_citation` rejects a mapping whose
//! `cited_object_schema()` does not target the object's schema.
//!
//! ```text
//! Fact ──► core/uploaded-blob-whole-v1     ──► core/uploaded-blob-v1
//! Fact ──► core/uploaded-blob-page-span-v1 ──► core/uploaded-blob-v1
//! ```
//!
//! Page spans are core rather than per-domain because the artefact they
//! locate into already is: a page range in an uploaded document says nothing
//! about what kind of document it is, in the same way `uploaded-blob-v1`
//! says nothing about what the bytes mean. What stays out of core is
//! anything needing a coordinate-system contract — a region on a page has to
//! agree with whoever produced the box about pixels, points, or fractions,
//! and that agreement belongs with the producer.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::{CitationMappingPayload, CitedObjectPayload, EntityKind, SchemaId};

/// Which memory kinds may hold a direct citation
/// (`memory.blob_id`), at multiplicity 0..1 each.
///
/// Fact and Abstraction, never Perspective. A Fact cites the artefact it
/// was read from. An Abstraction cites its proof — a computed score is
/// an Abstraction whose payload holds the value and the method and whose
/// citation is the computation record, which is what keeps such a score
/// from becoming an edge property or a cache row (docs/16 §Computed
/// Scores Are Abstractions). A Perspective never cites directly: an
/// interpretation grounds through the nodes it references, and a
/// bibliography of its own would be a second, competing ground.
///
/// Bibliographic closure for A/P therefore terminates at Fact citations
/// *and* direct Abstraction citations (amending docs/11 §Multiplicity).
#[must_use]
pub const fn kind_may_cite_directly(kind: EntityKind) -> bool {
    matches!(kind, EntityKind::Fact | EntityKind::Abstraction)
}

pub const UPLOADED_BLOB_SCHEMA_ID: &str = "core/uploaded-blob-v1";
pub const UPLOADED_BLOB_WHOLE_SCHEMA_ID: &str = "core/uploaded-blob-whole-v1";
pub const UPLOADED_BLOB_PAGE_SPAN_SCHEMA_ID: &str = "core/uploaded-blob-page-span-v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UploadedBlobPayload {
    pub content_hash: [u8; 32],
    pub bucket: String,
    pub object_key: String,
    pub sha256: [u8; 32],
    pub byte_len: u64,
    pub mime: String,
    pub filename: String,
    pub etag: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    #[schemars(with = "String")]
    pub uploaded_at: OffsetDateTime,
}

impl CitedObjectPayload for UploadedBlobPayload {
    const SCHEMA_ID: &'static str = UPLOADED_BLOB_SCHEMA_ID;
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        ""
    }

    fn idempotency_key(&self) -> [u8; 32] {
        self.content_hash
    }
}

/// "This Fact came from that artefact", with no locator inside it.
///
/// The common case, and a pure link: the whole mapping is
/// `memory.blob_id`, so there is no sidecar table (see
/// `CitationMappingPayload::sidecar_table`, which defaults to `None` —
/// do not mint an empty table to satisfy the trait).
///
/// A braced empty struct, not a unit struct, deliberately: serde
/// deserializes a unit struct from JSON `null` only, while the typed
/// ingest boundary (`FlavorRegistryFrozen::ingest_protocol_payload`)
/// requires every payload to be a JSON object — so as a unit struct this
/// mapping was unusable over MCP, with no payload a caller could pass.
/// The braced form accepts `{}` on the wire like every other mapping.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UploadedBlobWholeV1 {}

impl CitationMappingPayload for UploadedBlobWholeV1 {
    const SCHEMA_ID: &'static str = UPLOADED_BLOB_WHOLE_SCHEMA_ID;
    const SCHEMA_VERSION: u32 = 1;

    fn cited_object_schema() -> SchemaId {
        SchemaId::new(UPLOADED_BLOB_SCHEMA_ID.to_string())
    }
}

/// A page range inside a paginated artefact, optionally narrowed to a
/// character range within that range's extracted text.
///
/// One-based and inclusive on both ends, matching how a page is cited in
/// prose and printed on the page itself; a single page is
/// `page_from == page_to`. Zero-based would make "page 1" mean the second
/// page in every citation a human reads back.
///
/// The character range is optional and relative to the text of the span,
/// not of the document: a mapping that is re-derived after the document is
/// re-extracted stays meaningful as long as the pages did not move.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UploadedBlobPageSpanV1 {
    pub page_from: u32,
    pub page_to: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub char_range_start: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub char_range_end: Option<u32>,
}

impl UploadedBlobPageSpanV1 {
    /// A span covering one page.
    #[must_use]
    pub const fn page(page: u32) -> Self {
        Self {
            page_from: page,
            page_to: page,
            char_range_start: None,
            char_range_end: None,
        }
    }

    /// Whether this span is well formed.
    ///
    /// The sidecar's `CHECK` constraints are the authority — they hold for
    /// rows written by any client, including ones that never went through
    /// this type. This exists so a caller can reject a bad span before
    /// spending a write, not instead of them.
    #[must_use]
    pub const fn is_valid(&self) -> bool {
        if self.page_from < 1 || self.page_to < self.page_from {
            return false;
        }
        match (self.char_range_start, self.char_range_end) {
            (None, None) => true,
            (Some(start), Some(end)) => end >= start,
            _ => false,
        }
    }
}

impl CitationMappingPayload for UploadedBlobPageSpanV1 {
    const SCHEMA_ID: &'static str = UPLOADED_BLOB_PAGE_SPAN_SCHEMA_ID;
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> Option<&'static str> {
        None
    }

    fn cited_object_schema() -> SchemaId {
        SchemaId::new(UPLOADED_BLOB_SCHEMA_ID.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_span_is_one_based_and_inclusive() {
        assert!(UploadedBlobPageSpanV1::page(1).is_valid());
        assert!(!UploadedBlobPageSpanV1::page(0).is_valid());
        let span = UploadedBlobPageSpanV1 {
            page_from: 12,
            page_to: 11,
            char_range_start: None,
            char_range_end: None,
        };
        assert!(!span.is_valid(), "page_to before page_from");
    }

    #[test]
    fn a_char_range_is_present_at_both_ends_or_neither() {
        let half = UploadedBlobPageSpanV1 {
            page_from: 3,
            page_to: 3,
            char_range_start: Some(10),
            char_range_end: None,
        };
        assert!(!half.is_valid());
        let whole = UploadedBlobPageSpanV1 {
            page_from: 3,
            page_to: 3,
            char_range_start: Some(10),
            char_range_end: Some(10),
        };
        assert!(whole.is_valid(), "an empty range at one offset is a point");
    }

    #[test]
    fn both_mappings_target_the_uploaded_blob_object() {
        assert_eq!(
            UploadedBlobWholeV1::cited_object_schema().as_str(),
            UPLOADED_BLOB_SCHEMA_ID
        );
        assert_eq!(
            UploadedBlobPageSpanV1::cited_object_schema().as_str(),
            UPLOADED_BLOB_SCHEMA_ID
        );
    }
}
