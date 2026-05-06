-- M9 — text-search indexes for code-flavor MCP tools.

CREATE FUNCTION proxima_code.text_array_search(items text[])
RETURNS text
LANGUAGE sql
IMMUTABLE
PARALLEL SAFE
AS $$
    SELECT array_to_string(items, ' ')
$$;

CREATE INDEX idx_code_chunk_v1_text_search
    ON proxima_code.code_chunk_v1
    USING gin (to_tsvector('pg_catalog.simple'::regconfig, file_path || ' ' || text));

CREATE INDEX idx_file_revision_v1_path_search
    ON proxima_code.file_revision_v1
    USING gin (to_tsvector('pg_catalog.simple'::regconfig, file_path));

CREATE INDEX idx_commit_v1_message_search
    ON proxima_code.commit_v1
    USING gin (to_tsvector('pg_catalog.simple'::regconfig, sha || ' ' || message));

CREATE INDEX idx_commit_summary_v1_search
    ON proxima_code.commit_summary_v1
    USING gin (to_tsvector(
        'pg_catalog.simple'::regconfig,
        commit_sha || ' ' || summary || ' ' || proxima_code.text_array_search(key_files)
    ));
