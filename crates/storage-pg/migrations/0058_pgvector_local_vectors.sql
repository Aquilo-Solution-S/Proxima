-- The pgvector part of the schema, for a store that keeps its vectors in its
-- own database: the `vector` extension, the vector column of
-- proxima_core.embeddings, its width check and the per-width HNSW indexes.
-- A store whose vectors live in a host index never applies this file (the
-- migration planner skips every `pgvector_*` migration for it), so it needs
-- no `vector` extension. Immutable once released.
--
-- The embeddings table is empty here: a fresh store has written nothing, and
-- a store that kept its vectors in a host index wrote no row to it. That
-- makes the NOT NULL column safe to add.

CREATE EXTENSION IF NOT EXISTS vector;

ALTER TABLE proxima_core.embeddings
    ADD COLUMN vec public.vector NOT NULL;

ALTER TABLE proxima_core.embeddings
    ADD CONSTRAINT embeddings_vec_width_chk CHECK ((public.vector_dims(vec) = dim));

CREATE INDEX embeddings_hnsw_d1024 ON proxima_core.embeddings USING hnsw (((vec)::public.vector(1024)) public.vector_cosine_ops) WHERE (dim = 1024);

CREATE INDEX embeddings_hnsw_d1536 ON proxima_core.embeddings USING hnsw (((vec)::public.vector(1536)) public.vector_cosine_ops) WHERE (dim = 1536);

CREATE INDEX embeddings_hnsw_d2048 ON proxima_core.embeddings USING hnsw (((vec)::public.halfvec(2048)) public.halfvec_cosine_ops) WHERE (dim = 2048);

CREATE INDEX embeddings_hnsw_d3072 ON proxima_core.embeddings USING hnsw (((vec)::public.halfvec(3072)) public.halfvec_cosine_ops) WHERE (dim = 3072);

CREATE INDEX embeddings_hnsw_d384 ON proxima_core.embeddings USING hnsw (((vec)::public.vector(384)) public.vector_cosine_ops) WHERE (dim = 384);

CREATE INDEX embeddings_hnsw_d768 ON proxima_core.embeddings USING hnsw (((vec)::public.vector(768)) public.vector_cosine_ops) WHERE (dim = 768);
