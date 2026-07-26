-- Proxima core schema — v0.0.7 append-only migration (version 11).
--
-- 0001_init.sql is the SHIPPED v0.0.4 baseline (sqlx checksum-pinned, NEVER
-- edit). 0008/0009/0010 are the prior append-only lanes. Versions 2..7 are
-- RETIRED_PRE_V004_MIGRATION_VERSIONS (crates/storage-pg/src/lib.rs); SQLx
-- derives the version from the filename prefix, so the core sequence continues
-- at 11.

-- ---------------------------------------------------------------------------
-- Chunked embeddings.
-- A memory whose text exceeds the embedding provider's input limit used to go
-- terminally un-embedded (semantically invisible). It is now embedded as
-- multiple chunks under one embedding_version: chunk_index joins the primary
-- key, existing single-chunk rows keep chunk_index 0, and head semantics are
-- unchanged (heads still point at a version, never a chunk).
-- ---------------------------------------------------------------------------
ALTER TABLE proxima_core.embeddings
    ADD COLUMN chunk_index integer NOT NULL DEFAULT 0;

ALTER TABLE proxima_core.embeddings
    ADD CONSTRAINT embeddings_chunk_index_nonnegative_chk CHECK (chunk_index >= 0);

ALTER TABLE proxima_core.embeddings
    DROP CONSTRAINT embeddings_pkey;

ALTER TABLE proxima_core.embeddings
    ADD CONSTRAINT embeddings_pkey
    PRIMARY KEY (entity_kind, entity_id, embedding_version, model_id, chunk_index);

-- ---------------------------------------------------------------------------
-- Stored lexical tsvectors.
-- The lexical branch scrubbed punctuation and over-long tokens out of every
-- candidate's search text and ran to_tsvector over the result, on every
-- search, for every candidate. Measured on a 150k-memory corpus: one owner's
-- 427 candidates cost 48.7ms of the branch's 102ms mean, while fetching those
-- same rows cost 0.9ms. The work grew with how much text an owner held, not
-- with how much of it the query could possibly match.
--
-- The vector is a pure function of the row, so it is computed once at write
-- time and stored. proxima_core.lexical_tsv is the single definition of that
-- function: the generated columns below and the search builder's fallback
-- path (sidecars that have no stored column) both call it, so the two cannot
-- drift. The wrappers exist because concat_ws and array_to_string are marked
-- STABLE — correct for their variadic "any" forms, but they are genuinely
-- immutable over the text arguments used here, and a generated column will
-- not accept a STABLE expression.
--
-- No GIN index accompanies these columns, deliberately. 0009 dropped the
-- v0.0.6 GIN indexes because the planner cannot select an index on a base
-- table for a predicate applied to the owner-scoped candidates CTE, and that
-- has not changed: owner-first enumeration already reduces a search to a few
-- hundred rows before any text predicate runs. The win here is not avoiding a
-- scan, it is not recomputing a vector we could have kept. An index would add
-- write amplification and buy nothing at this plan shape.
--
-- ADD COLUMN ... GENERATED ALWAYS AS ... STORED rewrites each table and holds
-- ACCESS EXCLUSIVE for the duration (measured: 54.7s for a 149k-row memories
-- plus 24.8k-row sidecar).
-- ---------------------------------------------------------------------------
CREATE FUNCTION proxima_core.lexical_scrub(txt text) RETURNS text
LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE AS
$$ SELECT regexp_replace(
       regexp_replace(txt, '[[:punct:]]+', ' ', 'g'),
       '\m[[:alnum:]]{255}[[:alnum:]]+\M', ' ', 'g') $$;

COMMENT ON FUNCTION proxima_core.lexical_scrub(text) IS
'Punctuation and over-long-token scrub applied before lexical tokenisation. Kept in one place so stored columns and the query builder cannot diverge.';

CREATE FUNCTION proxima_core.lexical_tsv(txt text) RETURNS tsvector
LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE AS
$$ SELECT to_tsvector('english'::regconfig, proxima_core.lexical_scrub(txt)) $$;

COMMENT ON FUNCTION proxima_core.lexical_tsv(text) IS
'The canonical lexical vector for one text blob. STRICT so an absent search text stays NULL rather than becoming an empty tsvector, matching the builder expression it replaces.';

CREATE FUNCTION proxima_core.lexical_text_array(parts text[]) RETURNS text
LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE AS
$$ SELECT NULLIF(array_to_string(parts, ' '), '') $$;

CREATE FUNCTION proxima_core.lexical_join(VARIADIC parts text[]) RETURNS text
LANGUAGE sql IMMUTABLE PARALLEL SAFE AS
$$ SELECT NULLIF(concat_ws(' ', VARIADIC parts), '') $$;

COMMENT ON FUNCTION proxima_core.lexical_join(text[]) IS
'Immutable spelling of the projection concatenation the search builder emits: non-null parts joined by a single space, empty result folded to NULL.';

ALTER TABLE proxima_core.memories
    ADD COLUMN search_tsv tsvector
    GENERATED ALWAYS AS (proxima_core.lexical_tsv(COALESCE(text, ''))) STORED;

ALTER TABLE proxima_core.agent_derivation_v1
    ADD COLUMN search_tsv tsvector
    GENERATED ALWAYS AS (
        proxima_core.lexical_tsv(proxima_core.lexical_join(
            NULLIF(title, ''),
            NULLIF(body, ''),
            proxima_core.lexical_text_array(tags)))) STORED;

ALTER TABLE proxima_core.agent_note_v1
    ADD COLUMN search_tsv tsvector
    GENERATED ALWAYS AS (
        proxima_core.lexical_tsv(proxima_core.lexical_join(
            NULLIF(title, ''),
            NULLIF(body, ''),
            proxima_core.lexical_text_array(tags)))) STORED;
