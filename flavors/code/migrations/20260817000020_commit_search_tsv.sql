-- Store the commit/summary lexical vector at write so search is @@ on a
-- column, not to_tsvector in the WHERE. Route through the same
-- proxima_core.lexical_tsv / lexical_join builders as code_chunk_v1.
-- These tables have no lexical_language column; pin english like chunks.

ALTER TABLE proxima_code.commit_v1
    ADD COLUMN search_tsv tsvector
    GENERATED ALWAYS AS (
        proxima_core.lexical_tsv(
            'english'::regconfig,
            proxima_core.lexical_join(
                VARIADIC ARRAY[
                    NULLIF(sha, ''),
                    NULLIF(message, ''),
                    NULLIF(author_name, ''),
                    NULLIF(author_email, '')
                ]
            )
        )
    ) STORED;

DROP INDEX IF EXISTS proxima_code.idx_commit_v1_message_search;
CREATE INDEX idx_commit_v1_search_tsv
    ON proxima_code.commit_v1 USING gin (search_tsv);

ALTER TABLE proxima_code.commit_summary_v1
    ADD COLUMN search_tsv tsvector
    GENERATED ALWAYS AS (
        proxima_core.lexical_tsv(
            'english'::regconfig,
            proxima_core.lexical_join(
                VARIADIC ARRAY[
                    NULLIF(commit_sha, ''),
                    NULLIF(summary, ''),
                    proxima_core.lexical_text_array(key_files)
                ]
            )
        )
    ) STORED;

DROP INDEX IF EXISTS proxima_code.idx_commit_summary_v1_search;
CREATE INDEX idx_commit_summary_v1_search_tsv
    ON proxima_code.commit_summary_v1 USING gin (search_tsv);
