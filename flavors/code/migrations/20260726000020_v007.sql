-- Code flavor — v0.0.7 lane.
--
-- Stored lexical vector for code chunks.
--
-- `proxima-code_search_chunks` scored every surviving row with
-- `ts_rank_cd(to_tsvector('simple', file_path || ' ' || text), tsq)`. The
-- expression index below already served the *predicate*, but a GIN index is
-- lossy — Postgres cannot read a tsvector back out of it — so ranking
-- recomputed the vector for every matching row on every search. That is the
-- same defect core search carried until 0011_v007.sql, and the same fix:
-- the vector is a pure function of the row, so compute it once at write
-- time.
--
-- The config stays 'simple', deliberately, and this is NOT the 'english'
-- switch core made. Code is not English: stemming would fold `parsing` into
-- `pars` and collapse identifiers that differ only by suffix, and stopword
-- removal would drop `in`, `as`, `if`, `do`, `no`, `on` — all real Rust and
-- TypeScript tokens. Preserving 'simple' means this migration changes cost,
-- never results.
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
        to_tsvector('simple'::regconfig, file_path || ' ' || text)
    ) STORED;

COMMENT ON COLUMN proxima_code.code_chunk_v1.search_tsv IS
'Lexical vector over file_path + text, config ''simple'' so code identifiers are neither stemmed nor treated as stopwords. Must stay identical to the expression proxima-code_search_chunks matches against.';

CREATE INDEX idx_code_chunk_v1_search_tsv
    ON proxima_code.code_chunk_v1 USING gin (search_tsv);

DROP INDEX IF EXISTS proxima_code.idx_code_chunk_v1_text_search;
