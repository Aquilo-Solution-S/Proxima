-- One embedding version contains every text chunk; legacy vectors are chunk zero.
ALTER TABLE proxima_core.embeddings
    ADD COLUMN chunk_ordinal integer NOT NULL DEFAULT 0,
    ADD CONSTRAINT embeddings_chunk_ordinal_chk CHECK (chunk_ordinal >= 0);

ALTER TABLE proxima_core.embeddings
    DROP CONSTRAINT embeddings_pkey,
    ADD CONSTRAINT embeddings_pkey
        PRIMARY KEY (entity_id, model_id, dim, embedding_version, chunk_ordinal);
