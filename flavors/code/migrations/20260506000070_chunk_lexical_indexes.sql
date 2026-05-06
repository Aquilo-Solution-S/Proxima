-- M10 — substring indexes for code_search_chunks lexical boosts.

CREATE EXTENSION IF NOT EXISTS pg_trgm;

CREATE INDEX idx_code_chunk_v1_file_path_trgm
    ON proxima_code.code_chunk_v1
    USING gin (lower(file_path) gin_trgm_ops);

CREATE INDEX idx_code_chunk_v1_text_trgm
    ON proxima_code.code_chunk_v1
    USING gin (lower(text) gin_trgm_ops);

CREATE INDEX idx_code_chunk_v1_chunk_type
    ON proxima_code.code_chunk_v1 (chunk_type);
