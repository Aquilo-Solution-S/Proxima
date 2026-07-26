-- Code flavor — v0.0.7 lane.
--
-- Stored lexical vector for code chunks.
--
-- `proxima-code_search_chunks` scored every surviving row with
-- `ts_rank_cd(to_tsvector(..., file_path || ' ' || text), tsq)`. The
-- expression index below already served the *predicate*, but a GIN index is
-- lossy — Postgres cannot read a tsvector back out of it — so ranking
-- recomputed the vector for every matching row on every search. That is the
-- same defect core search carried until 0011_v007.sql, and the same fix:
-- the vector is a pure function of the row, so compute it once at write time.
--
-- The vector is `proxima_core.lexical_tsv` over `proxima_core.lexical_join`,
-- the same pair core's stored columns use, for two reasons.
--
-- 1. It is the only config under which a natural-language query can match.
--    `simple` neither stems nor drops stopwords, so
--    `websearch_to_tsquery('simple', 'how does the code chunker decide how
--    big a chunk should be')` is a twelve-lexeme AND including 'how', 'the',
--    'a' and 'be' — nothing in any corpus satisfies it, and an OR-rescue over
--    those same lexemes matches everything. Measured on this repository's own
--    index: 0 of 24 natural-language queries returned a single row. Under
--    `english` the same question reduces to `code & chunker & decid & big &
--    chunk`, which a rescue arm can use. `lexical_scrub` (inside
--    `lexical_tsv`) additionally rewrites punctuation to spaces on both
--    sides, so `embed_in_chunks` tokenises identically whether it arrives as
--    an identifier or as three words.
--
--    Stemming does fold `parsing`/`parsed`/`parser` together, and English
--    stopwords do include the real keywords `in`, `as`, `if`, `do`, `no`,
--    `on`. Exact identifier and keyword lookup is served by the substring
--    arm of the search — which carries a larger score bonus than any rank —
--    not by the tsvector, so that precision is not lost, only relocated.
--
-- 2. It makes one vector serve both search surfaces. Because the expression
--    is exactly `lexical_tsv(lexical_join(<projected fields>))`,
--    `CodeChunkV1::search_projection()` can name this column as its
--    `tsv_column` and core's `core_search_memories` reads the same stored
--    vector it would otherwise compute inline. A column built with a
--    different config could not be shared: core always builds its tsquery
--    with `english`, so a `simple` column would silently match nothing that
--    stems and everything that does not.
--
-- The old expression index is replaced by a plain GIN over the stored
-- column. Note the contrast with core, where 0011 deliberately adds no GIN:
-- there the text predicate applies to an owner-scoped `candidates` CTE that
-- no base-table index can serve. Here the predicate sits directly on
-- `code_chunk_v1`, so an index on it is exactly what the planner can use.
--
-- ADD COLUMN ... GENERATED ALWAYS AS ... STORED rewrites the table and holds
-- ACCESS EXCLUSIVE for the duration, proportional to indexed corpus size.
-- See MIGRATING.md's v0.0.7 lane.

ALTER TABLE proxima_code.code_chunk_v1
    ADD COLUMN search_tsv tsvector
    GENERATED ALWAYS AS (
        proxima_core.lexical_tsv(proxima_core.lexical_join(
            NULLIF(file_path, ''),
            NULLIF(text, '')))
    ) STORED;

COMMENT ON COLUMN proxima_code.code_chunk_v1.search_tsv IS
'Lexical vector over file_path + text via proxima_core.lexical_tsv, so code chunks share core''s text-search config and CodeChunkV1::search_projection() can name this column as its tsv_column. Must stay identical to lexical_tsv(lexical_join(<projected fields>)).';

CREATE INDEX idx_code_chunk_v1_search_tsv
    ON proxima_code.code_chunk_v1 USING gin (search_tsv);

DROP INDEX IF EXISTS proxima_code.idx_code_chunk_v1_text_search;
