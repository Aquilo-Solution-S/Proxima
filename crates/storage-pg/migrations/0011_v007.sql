-- v007: chunked embeddings.
--
-- A memory whose text exceeds the embedding provider's input limit used to
-- go terminally un-embedded (semantically invisible). It is now embedded as
-- multiple chunks under one embedding_version: chunk_index joins the primary
-- key, existing single-chunk rows keep chunk_index 0, and head semantics are
-- unchanged (heads still point at a version, never a chunk).

ALTER TABLE proxima_core.embeddings
    ADD COLUMN chunk_index integer NOT NULL DEFAULT 0;

ALTER TABLE proxima_core.embeddings
    ADD CONSTRAINT embeddings_chunk_index_nonnegative_chk CHECK (chunk_index >= 0);

ALTER TABLE proxima_core.embeddings
    DROP CONSTRAINT embeddings_pkey;

ALTER TABLE proxima_core.embeddings
    ADD CONSTRAINT embeddings_pkey
    PRIMARY KEY (entity_kind, entity_id, embedding_version, model_id, chunk_index);
