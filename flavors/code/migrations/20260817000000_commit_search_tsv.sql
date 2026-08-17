-- Store the commit/summary lexical vector at write so search is @@ on a
-- column, not to_tsvector in the WHERE. Same simple config the live
-- search query already uses.

ALTER TABLE proxima_code.commit_v1
    ADD COLUMN search_tsv tsvector
    GENERATED ALWAYS AS (
        to_tsvector('pg_catalog.simple'::regconfig, (sha || ' ' || message))
    ) STORED;

DROP INDEX IF EXISTS proxima_code.idx_commit_v1_message_search;
CREATE INDEX idx_commit_v1_search_tsv
    ON proxima_code.commit_v1 USING gin (search_tsv);

ALTER TABLE proxima_code.commit_summary_v1
    ADD COLUMN search_tsv tsvector
    GENERATED ALWAYS AS (
        to_tsvector(
            'pg_catalog.simple'::regconfig,
            ((commit_sha || ' ') || summary || ' ') || proxima_code.text_array_search(key_files)
        )
    ) STORED;

DROP INDEX IF EXISTS proxima_code.idx_commit_summary_v1_search;
CREATE INDEX idx_commit_summary_v1_search_tsv
    ON proxima_code.commit_summary_v1 USING gin (search_tsv);
