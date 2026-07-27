-- Proxima core schema — v0.0.7 append-only migration (version 13).
--
-- 0001_init.sql is the SHIPPED v0.0.4 baseline (sqlx checksum-pinned, NEVER
-- edit). 0008..0012 are the prior append-only lanes.

-- ---------------------------------------------------------------------------
-- Documents become citable, and citable by page.
--
-- `core/uploaded-blob-v1` has been a registered CitedObject schema since the
-- baseline, and the S3 upload lane has been writing rows into
-- cited_uploaded_blob_v1 the whole time. But no registered
-- CitationMappingPayload named it as its cited_object_schema(), and a
-- mapping is the only path from a Fact to a cited object
-- (memories.citation_mapping_id). `authorize_fact_with_citation` checks that
-- the mapping schema targets the object's schema, so there was no argument a
-- caller could pass that would attach a Fact to an uploaded blob. Core
-- shipped an upload lane whose artefacts nothing could cite.
--
-- Two mappings close it. `core/uploaded-blob-whole-v1` is a pure link and
-- needs no table (see the CitationMappingPayload contract — a fieldless
-- mapping returns None rather than minting an empty table).
-- `core/uploaded-blob-page-span-v1` is the locator docs/11 has always
-- described, and needs the table below.
--
-- Page numbers are one-based and inclusive at both ends: that is how a page
-- is cited in prose and how it is printed on the page. Zero-based would make
-- "page 1" mean the second page in every citation a human reads back.
-- ---------------------------------------------------------------------------

CREATE TABLE proxima_core.citation_uploaded_blob_page_span_v1 (
    citation_mapping_id uuid PRIMARY KEY
        REFERENCES proxima_core.citation_mappings(citation_mapping_id)
        ON DELETE CASCADE,
    page_from integer NOT NULL,
    page_to integer NOT NULL,
    char_range_start integer,
    char_range_end integer,
    CONSTRAINT citation_blob_page_span_pages_chk
        CHECK (page_from >= 1 AND page_to >= page_from),
    -- Both ends or neither: a half-open character range cannot be resolved
    -- back to a substring, and silently treating a missing end as "to the
    -- end of the span" would make two different citations compare equal.
    CONSTRAINT citation_blob_page_span_chars_chk
        CHECK (
            (char_range_start IS NULL) = (char_range_end IS NULL)
            AND (char_range_start IS NULL
                 OR (char_range_start >= 0 AND char_range_end >= char_range_start))
        )
);

COMMENT ON TABLE proxima_core.citation_uploaded_blob_page_span_v1 IS
'Sidecar for core/uploaded-blob-page-span-v1: which pages of a cited uploaded document a Fact came from. Pages are one-based and inclusive at both ends; a single page has page_from = page_to. char_range_* is optional and relative to the text of the span, not of the document. See docs/11-citations.md.';

-- Read pattern: "which Facts cite pages of this document, in page order".
-- The citation_mapping_id primary key answers the reverse direction only.
CREATE INDEX idx_citation_blob_page_span_pages
    ON proxima_core.citation_uploaded_blob_page_span_v1 (page_from, page_to);
