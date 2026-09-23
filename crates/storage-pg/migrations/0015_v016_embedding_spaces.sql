-- v0.0.16 embedding spaces: one untyped vector column, one HNSW lane per width.
--
-- An embedding space is `(model_id, dim)`. Until now `vec` was `vector(1024)`
-- under one global HNSW index, so every deployment embedded at exactly one
-- width. pgvector cannot index an untyped column, but it can index an
-- expression over it: each supported width gets a partial expression index
-- `(vec::vector(N)) WHERE dim = N`, and a query that names the same
-- expression and the literal predicate is served by that lane alone.
--
-- The widths are closed (`proxima_core::llm::EmbeddingDim`); the CHECKs below
-- are the same list, so a width the binary cannot index can never be stored.
-- Widths above 2000 index as `halfvec`: pgvector's HNSW cap for `vector` is
-- 2000 dimensions, 4000 for `halfvec`. Stored values stay full precision.
--
-- Cost: `vec` only loses its typmod, which is not a rewrite. The 1024 lane
-- index is rebuilt here over every existing row, the same build the dropped
-- index took; the other lanes are empty. The CHECKs scan once.

DO $embedding_spaces_pgvector$
DECLARE
    version_parts text[];
BEGIN
    SELECT regexp_match(extversion, '^(\d+)\.(\d+)')
      INTO version_parts
      FROM pg_extension
     WHERE extname = 'vector';
    IF version_parts IS NULL
       OR (version_parts[1]::int, version_parts[2]::int) < (0, 8) THEN
        RAISE EXCEPTION 'embedding width lanes require pgvector >= 0.8.0';
    END IF;
END
$embedding_spaces_pgvector$;

DROP INDEX proxima_core.idx_embeddings_vec_hnsw;
ALTER TABLE proxima_core.embeddings ALTER COLUMN vec TYPE vector;

-- Existing rows are all 1024 wide. The default fills them without a rewrite
-- and is then dropped, so every writer must name the width.
ALTER TABLE proxima_core.embeddings ADD COLUMN dim smallint NOT NULL DEFAULT 1024;
ALTER TABLE proxima_core.embedding_heads ADD COLUMN dim smallint NOT NULL DEFAULT 1024;
ALTER TABLE proxima_core.embedding_jobs ADD COLUMN dim smallint NOT NULL DEFAULT 1024;
ALTER TABLE proxima_core.embeddings ALTER COLUMN dim DROP DEFAULT;
ALTER TABLE proxima_core.embedding_heads ALTER COLUMN dim DROP DEFAULT;
ALTER TABLE proxima_core.embedding_jobs ALTER COLUMN dim DROP DEFAULT;

ALTER TABLE proxima_core.embeddings
    ADD CONSTRAINT embeddings_dim_lane_chk
        CHECK (dim IN (384, 768, 1024, 1536, 2048, 3072)),
    ADD CONSTRAINT embeddings_vec_width_chk
        CHECK (vector_dims(vec) = dim);
ALTER TABLE proxima_core.embedding_heads
    ADD CONSTRAINT embedding_heads_dim_lane_chk
        CHECK (dim IN (384, 768, 1024, 1536, 2048, 3072));
ALTER TABLE proxima_core.embedding_jobs
    ADD CONSTRAINT embedding_jobs_dim_lane_chk
        CHECK (dim IN (384, 768, 1024, 1536, 2048, 3072));

-- The width is part of the space, so it is part of every key: the same model
-- at a second width is a second space, not a collision.
ALTER TABLE proxima_core.embeddings
    DROP CONSTRAINT embeddings_pkey,
    ADD CONSTRAINT embeddings_pkey
        PRIMARY KEY (entity_id, model_id, dim, embedding_version);
ALTER TABLE proxima_core.embedding_heads
    DROP CONSTRAINT embedding_heads_pkey,
    ADD CONSTRAINT embedding_heads_pkey PRIMARY KEY (entity_id, model_id, dim);
ALTER TABLE proxima_core.embedding_jobs
    DROP CONSTRAINT embedding_jobs_owner_id_entity_id_model_id_key,
    ADD CONSTRAINT embedding_jobs_owner_id_entity_id_model_id_dim_key
        UNIQUE (owner_id, entity_id, model_id, dim);

DROP INDEX proxima_core.embeddings_owner_model_idx;
CREATE INDEX embeddings_owner_model_idx
    ON proxima_core.embeddings (owner_id, model_id, dim);

DROP INDEX proxima_core.embedding_jobs_pending_claim_idx;
CREATE INDEX embedding_jobs_pending_claim_idx
    ON proxima_core.embedding_jobs (model_id, dim, job_id)
    WHERE status = 'pending';

CREATE INDEX embeddings_hnsw_d384 ON proxima_core.embeddings
    USING hnsw ((vec::vector(384)) vector_cosine_ops) WHERE dim = 384;
CREATE INDEX embeddings_hnsw_d768 ON proxima_core.embeddings
    USING hnsw ((vec::vector(768)) vector_cosine_ops) WHERE dim = 768;
CREATE INDEX embeddings_hnsw_d1024 ON proxima_core.embeddings
    USING hnsw ((vec::vector(1024)) vector_cosine_ops) WHERE dim = 1024;
CREATE INDEX embeddings_hnsw_d1536 ON proxima_core.embeddings
    USING hnsw ((vec::vector(1536)) vector_cosine_ops) WHERE dim = 1536;
CREATE INDEX embeddings_hnsw_d2048 ON proxima_core.embeddings
    USING hnsw ((vec::halfvec(2048)) halfvec_cosine_ops) WHERE dim = 2048;
CREATE INDEX embeddings_hnsw_d3072 ON proxima_core.embeddings
    USING hnsw ((vec::halfvec(3072)) halfvec_cosine_ops) WHERE dim = 3072;
