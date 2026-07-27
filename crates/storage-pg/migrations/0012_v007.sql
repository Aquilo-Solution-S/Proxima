-- Proxima core schema — v0.0.7 append-only migration (version 12).
--
-- 0001_init.sql is the SHIPPED v0.0.4 baseline (sqlx checksum-pinned, NEVER
-- edit). 0008..0011 are the prior append-only lanes.

-- ---------------------------------------------------------------------------
-- The lexical text-search configuration becomes a property of the database.
--
-- Until now `english` was written in two places that had to agree and had no
-- way to check that they did: `proxima_core.lexical_tsv` (0011), which every
-- stored `search_tsv` column is generated from, and a Rust constant the query
-- builder used to construct `websearch_to_tsquery`. Document side and query
-- side must use the *same* configuration or they stop matching — a german
-- vector answered by an english tsquery is not a worse search, it is a broken
-- one.
--
-- That coupling is why the configuration cannot simply be a setting the
-- caller passes: `lexical_tsv` is IMMUTABLE because generated columns require
-- it, so the value has to be frozen into the function. Making it a function
-- of its own keeps one authority for both sides — the query builder now emits
-- `proxima_core.lexical_config()` instead of a literal, so the two cannot
-- drift by construction.
--
-- Why this matters beyond tidiness: measured on 2,350 pages of German
-- technical literature with 130 verified questions, switching this one value
-- from `english` to `german` moved recall@5 from 0.438 to 0.577 and MRR from
-- 0.349 to 0.490. On the third of questions that do not reuse their source
-- page's wording — the ones that actually test retrieval rather than string
-- overlap — recall@5 went from 0.068 to 0.250. `english` on German text keeps
-- *der/und/für* as content words and never conflates
-- Bauleitung/Bauleitungen.
--
-- The default stays `english`. This migration changes no stored vector.
-- ---------------------------------------------------------------------------

CREATE FUNCTION proxima_core.lexical_config() RETURNS regconfig
LANGUAGE sql IMMUTABLE PARALLEL SAFE AS
$$ SELECT 'english'::regconfig $$;

COMMENT ON FUNCTION proxima_core.lexical_config() IS
'The one text-search configuration this database tokenises and queries with. Read by proxima_core.lexical_tsv for stored vectors and emitted by the search builder for the query tsquery, so document side and query side cannot disagree. Change it only through proxima_core.set_lexical_config().';

-- Same vector as before for `english`; the configuration is now read rather
-- than inlined. Replacing the body is permitted while generated columns
-- depend on it, and stored values stay valid because the result is unchanged.
CREATE OR REPLACE FUNCTION proxima_core.lexical_tsv(txt text) RETURNS tsvector
LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE AS
$$ SELECT to_tsvector(proxima_core.lexical_config(), proxima_core.lexical_scrub(txt)) $$;

-- ---------------------------------------------------------------------------
-- Switching the configuration has to rewrite every stored vector, atomically.
--
-- PostgreSQL permits `CREATE OR REPLACE FUNCTION` on a function a stored
-- generated column depends on, and does not recompute the column. Redefining
-- `lexical_config()` by hand therefore leaves the table split-brained:
-- verified on a two-row table, the pre-existing row kept its english vector
-- ('die':1,4 — a German stopword indexed as content) while a row inserted
-- afterwards got the german one ('bauleit':2). No error is raised at any
-- point, and half the corpus silently stops being reachable by the other
-- half's queries.
--
-- So the switch is offered as one operation that does both, and it finds its
-- work by introspection rather than by a hardcoded table list — flavor
-- sidecars generate `search_tsv` from the same function (see
-- flavors/code/migrations/20260726000020_v007.sql) and must be rebuilt too,
-- but core cannot know their names.
--
-- Cost is a full table rewrite under ACCESS EXCLUSIVE per affected table
-- (0011 measured 54.7s for 149k memories plus a 24.8k-row sidecar), so this
-- is a maintenance-window operation, not a runtime setting.
-- ---------------------------------------------------------------------------

CREATE FUNCTION proxima_core.set_lexical_config(new_config regconfig)
RETURNS TABLE (rebuilt_schema text, rebuilt_table text, rebuilt_column text)
LANGUAGE plpgsql AS
$$
DECLARE
    target record;
BEGIN
    IF new_config IS NULL THEN
        RAISE EXCEPTION 'lexical configuration must not be null';
    END IF;

    EXECUTE format(
        'CREATE OR REPLACE FUNCTION proxima_core.lexical_config() RETURNS regconfig '
        'LANGUAGE sql IMMUTABLE PARALLEL SAFE AS $body$ SELECT %L::regconfig $body$',
        new_config::text);

    -- pg_catalog rather than information_schema: the latter hides columns
    -- from roles without privileges on the table, and a silently skipped
    -- table is exactly the split-brain this function exists to prevent.
    FOR target IN
        SELECT n.nspname AS s, c.relname AS t, a.attname AS col,
               pg_get_expr(d.adbin, d.adrelid) AS expr
          FROM pg_attribute a
          JOIN pg_class     c ON c.oid = a.attrelid
          JOIN pg_namespace n ON n.oid = c.relnamespace
          JOIN pg_attrdef   d ON d.adrelid = a.attrelid AND d.adnum = a.attnum
         WHERE a.attgenerated = 's'
           AND NOT a.attisdropped
           AND pg_get_expr(d.adbin, d.adrelid) ~ 'lexical_(tsv|config)'
         ORDER BY n.nspname, c.relname, a.attname
    LOOP
        -- Re-applying the identical expression is what forces the rewrite;
        -- the expression's *value* changed because lexical_config() did.
        EXECUTE format('ALTER TABLE %I.%I ALTER COLUMN %I SET EXPRESSION AS (%s)',
                       target.s, target.t, target.col, target.expr);
        rebuilt_schema := target.s;
        rebuilt_table  := target.t;
        rebuilt_column := target.col;
        RETURN NEXT;
    END LOOP;
END
$$;

COMMENT ON FUNCTION proxima_core.set_lexical_config(regconfig) IS
'Switch the database-wide lexical text-search configuration and rebuild every stored tsvector generated from it, including flavor sidecars, in one transaction. Returns the columns it rebuilt. Rewrites each affected table under ACCESS EXCLUSIVE — run it in a maintenance window, not at runtime.';
