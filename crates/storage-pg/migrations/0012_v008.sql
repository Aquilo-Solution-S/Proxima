-- v008: stored lexical tsvectors.
--
-- The lexical branch scrubs punctuation and over-long tokens out of every
-- candidate's search text and runs to_tsvector over the result. Both are
-- pure per-row CPU, paid on every search, for every candidate. Measured on
-- the 150k-memory bench corpus: one owner's 427 candidates cost 48.7ms of
-- the branch's 102ms mean, while fetching those same rows cost 0.9ms. The
-- work is quadratic in the wrong thing — it grows with how much text an
-- owner holds, not with how much of it the query could possibly match.
--
-- The vector is a pure function of the row, so it is computed once at write
-- time and stored. proxima_core.lexical_tsv is the single definition of
-- that function: the generated columns below and the search builder's
-- fallback path (sidecars that have no stored column) both call it, so the
-- two cannot drift. The wrappers exist because concat_ws and
-- array_to_string are marked STABLE — correct for their variadic "any"
-- forms, but they are genuinely immutable over the text arguments used
-- here, and a generated column will not accept a STABLE expression.
--
-- No GIN index accompanies these columns, deliberately. 0009 dropped the
-- v0.0.6 GIN indexes because the planner cannot select an index on a base
-- table for a predicate applied to the owner-scoped candidates CTE, and
-- that has not changed: owner-first enumeration already reduces a search to
-- a few hundred rows before any text predicate runs. The win here is not
-- avoiding a scan, it is not recomputing a vector we could have kept. An
-- index would add write amplification and buy nothing at this plan shape.
--
-- ADD COLUMN ... GENERATED ALWAYS AS ... STORED rewrites each table and
-- holds ACCESS EXCLUSIVE for the duration.

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
