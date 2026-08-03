-- Proxima core schema — the v0.0.7 lane (version 11).
--
-- 0001_init.sql is the SHIPPED v0.0.4 baseline (sqlx checksum-pinned, NEVER
-- edit). 0008/0009/0010 are the prior append-only lanes and shipped in v0.0.6.
-- Versions 2..7 are RETIRED_PRE_V004_MIGRATION_VERSIONS
-- (crates/storage-pg/src/lib.rs); SQLx derives the version from the filename
-- prefix, so the core sequence continues at 11.
--
-- This file is the WHOLE of what v0.0.7 adds to a v0.0.6 database. It was
-- authored as five files (11..15) during the cycle and folded into one before
-- the tag, so it is final-state DDL and not their concatenation: the stored
-- vectors are generated in their two-argument form on the first try rather
-- than created one-argument and rebound, `lexical_tsv`/`lexical_config`/
-- `set_lexical_config` appear once in the shape they end up in, and versions
-- 12..15 are RETIRED_V007_LANE_MIGRATION_VERSIONS. Nothing here was ever in a
-- tagged release, so the only databases that applied the five-file lane are
-- dev and staging ones; they fail closed at boot on the changed version-11
-- checksum and reset (`cargo run -p dev-migrate -- reset`). See MIGRATING.md.
--
-- The DROPs in the edge section below remove v0.0.4 BASELINE objects, not
-- anything this file creates — a fresh database never creates and then drops.

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
-- ---------------------------------------------------------------------------
CREATE FUNCTION proxima_core.lexical_scrub(txt text) RETURNS text
LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE AS
$$ SELECT regexp_replace(
       regexp_replace(txt, '[[:punct:]]+', ' ', 'g'),
       '\m[[:alnum:]]{255}[[:alnum:]]+\M', ' ', 'g') $$;

COMMENT ON FUNCTION proxima_core.lexical_scrub(text) IS
'Punctuation and over-long-token scrub applied before lexical tokenisation. Kept in one place so stored columns and the query builder cannot diverge.';

-- ---------------------------------------------------------------------------
-- The default lexical text-search configuration is a property of the database.
--
-- `english` used to be written in two places that had to agree and had no way
-- to check that they did: the vector function every stored `search_tsv` is
-- generated from, and a Rust constant the query builder used to construct
-- `websearch_to_tsquery`. Document side and query side must use the *same*
-- configuration or they stop matching — a german vector answered by an english
-- tsquery is not a worse search, it is a broken one.
--
-- That coupling is why the configuration cannot simply be a setting the caller
-- passes: the vector function is IMMUTABLE because generated columns require
-- it, so the value has to be frozen into a function. The query builder emits
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
-- The default stays `english`. Per-row languages are below; this function is
-- what a row gets when it does not say.
-- ---------------------------------------------------------------------------
CREATE FUNCTION proxima_core.lexical_config() RETURNS regconfig
LANGUAGE sql IMMUTABLE PARALLEL SAFE AS
$$ SELECT 'english'::regconfig $$;

COMMENT ON FUNCTION proxima_core.lexical_config() IS
'The DEFAULT text-search configuration: used for rows written without an explicit or detected language, and as the query-side fallback when lexical_languages is empty. Since v14 the per-row memories.lexical_language is authoritative for each stored vector. Change only through proxima_core.set_lexical_config().';

CREATE FUNCTION proxima_core.lexical_tsv(txt text) RETURNS tsvector
LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE AS
$$ SELECT to_tsvector(proxima_core.lexical_config(), proxima_core.lexical_scrub(txt)) $$;

COMMENT ON FUNCTION proxima_core.lexical_tsv(text) IS
'The canonical lexical vector for one text blob. STRICT so an absent search text stays NULL rather than becoming an empty tsvector, matching the builder expression it replaces.';

-- The two-argument vector: genuinely immutable — nothing in it reads mutable
-- state. The one-argument form above stays as the default-language spelling
-- for callers without a language column and as the boot marker.
CREATE FUNCTION proxima_core.lexical_tsv(config regconfig, txt text) RETURNS tsvector
LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE AS
$$ SELECT to_tsvector(config, proxima_core.lexical_scrub(txt)) $$;

COMMENT ON FUNCTION proxima_core.lexical_tsv(regconfig, text) IS
'The canonical lexical vector for one text blob under an explicit text-search configuration. Stored search_tsv columns generate through this with the row''s lexical_language, so the language a text was tokenised with is data on the row, not a function definition that can drift.';

CREATE FUNCTION proxima_core.lexical_text_array(parts text[]) RETURNS text
LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE AS
$$ SELECT NULLIF(array_to_string(parts, ' '), '') $$;

CREATE FUNCTION proxima_core.lexical_join(VARIADIC parts text[]) RETURNS text
LANGUAGE sql IMMUTABLE PARALLEL SAFE AS
$$ SELECT NULLIF(concat_ws(' ', VARIADIC parts), '') $$;

COMMENT ON FUNCTION proxima_core.lexical_join(text[]) IS
'Immutable spelling of the projection concatenation the search builder emits: non-null parts joined by a single space, empty result folded to NULL.';

-- ---------------------------------------------------------------------------
-- The lexical language is a property of the ROW; the database default above is
-- what a row gets when it does not say.
--
-- One database-wide value is the wrong shape the moment a corpus mixes
-- languages, which real corpora do: a German handbook and an English design
-- doc in one deployment cannot share a stemmer. Measured on 2,350 pages of
-- German technical literature with 130 verified questions, the wrong
-- configuration answers 1 in 15 of the questions that do not reuse the source
-- page's wording; the right one answers 1 in 4.
--
-- Making the language a `regconfig` column works because the two-argument
-- `to_tsvector(regconfig, text)` is IMMUTABLE, so a stored generated column
-- may read a sibling column for its configuration (verified on PG 18.4). The
-- column must be TYPE regconfig: a text column with a `::regconfig` cast is
-- rejected — the cast resolves names through the search_path and is only
-- STABLE.
--
-- It also retires a hazard by construction. PostgreSQL permits `CREATE OR
-- REPLACE FUNCTION` on a function a stored generated column depends on and
-- does not recompute the column, so a database-wide switch alone leaves the
-- table split-brained: verified on a two-row table, the pre-existing row kept
-- its english vector ('die':1,4 — a German stopword indexed as content) while
-- a row inserted afterwards got the german one ('bauleit':2), with no error
-- raised at any point. With the language as a column VALUE, PostgreSQL sees
-- the dependency: the vector is recomputed from the row itself, and there is
-- no function redefinition to go stale against.
--
-- Query-side design, measured before it was built (130-question goldset,
-- production band SQL): MATCH with the OR of one tsquery per active language
-- (constants — GIN-indexable; `websearch_to_tsquery(lang_column, q)` in a
-- WHERE clause has no index path), RANK each row with its own language's
-- tsquery. Ranked that way the mixed-language OR is bit-identical to the
-- single-language baseline (0/130 top-5 sets changed); ranked with the OR
-- query instead it costs 6.2 points of recall@5. The active-language set
-- lives in `lexical_languages` below.
-- ---------------------------------------------------------------------------

-- ---------------------------------------------------------------------------
-- The set of languages this database actively holds rows in. The query
-- builder ORs one tsquery per row of this table (a query cannot know its own
-- language, and with per-row ranking the OR is measured free), so membership
-- here is what makes a language *searchable*. Rows are added implicitly the
-- first time a write stamps their language, and by set_lexical_config for
-- the default.
-- ---------------------------------------------------------------------------
CREATE TABLE proxima_core.lexical_languages (
    config   regconfig   NOT NULL PRIMARY KEY,
    added_at timestamptz NOT NULL DEFAULT now()
);

COMMENT ON TABLE proxima_core.lexical_languages IS
'Active lexical languages: every configuration any row is tokenised with. The search builder builds one tsquery per entry and ORs them for matching (ranking is per-row). Remove entries only through proxima_core.lexical_language_forget, which refuses while rows still reference the configuration — DROP TEXT SEARCH CONFIGURATION is not blocked by stored regconfig values and leaves dangling OIDs that make rows fail on UPDATE.';

INSERT INTO proxima_core.lexical_languages (config)
VALUES (proxima_core.lexical_config());

-- OR-combine for the per-language tsqueries. `tsquery_or` is the function
-- behind `||`; it is STRICT, so NULL per-language queries (a query that is
-- all stopwords under one configuration) drop out of the aggregate instead
-- of poisoning it. The same function serves as combiner: OR is associative.
CREATE AGGREGATE proxima_core.tsquery_or_agg(tsquery) (
    SFUNC = pg_catalog.tsquery_or,
    STYPE = tsquery,
    COMBINEFUNC = pg_catalog.tsquery_or,
    PARALLEL = SAFE
);

COMMENT ON AGGREGATE proxima_core.tsquery_or_agg(tsquery) IS
'OR-combines tsqueries, used by the search builder to build one match query across every active lexical language. Empty tsqueries fold away; an empty input set yields NULL (callers COALESCE to the default configuration''s query).';

-- ---------------------------------------------------------------------------
-- Query text for one language arm of the search OR.
--
-- `simple` has no stop list, so one `simple`-stamped row in the database
-- (one reliably-detected CJK note is enough) would otherwise make every
-- query's FUNCTION words into match terms: plainto_tsquery('simple',
-- 'what is the plan') contributes 'is'|'the' to the rescue OR, and any
-- mixed-language row whose vector happens to carry an incidental English
-- function word rescue-matches unrelated questions above the substring
-- band (measured 0.328 for a CJK note matching an English question solely
-- via ''is''). So for configurations without a stop list, the query text is
-- first filtered through the DEFAULT configuration's stop list — content
-- words and CJK tokens survive untouched, function words never become
-- match terms. Stop-listed configurations pass through unchanged.
-- ---------------------------------------------------------------------------
CREATE FUNCTION proxima_core.lexical_query_text(config regconfig, query_text text)
RETURNS text
LANGUAGE sql STABLE PARALLEL SAFE AS
$$ SELECT CASE
       WHEN config = 'simple'::regconfig THEN
           (SELECT COALESCE(string_agg(tok, ' '), '')
              FROM regexp_split_to_table(query_text, '\s+') AS tok
             WHERE tok <> ''
               AND to_tsvector(proxima_core.lexical_config(), tok) <> '')
       ELSE query_text
   END $$;

COMMENT ON FUNCTION proxima_core.lexical_query_text(regconfig, text) IS
'The query text to parse under one configuration when building the cross-language match OR. For stop-list-free configurations (simple), tokens that the default configuration treats as stopwords are removed first, so function words never become match terms; every other configuration receives the text unchanged. Query-side only — stored vectors keep every token their row''s configuration keeps.';

-- ---------------------------------------------------------------------------
-- Per-row language columns, then the stored vectors that read them.
--
-- `DEFAULT proxima_core.lexical_config()` is non-volatile, so ADD COLUMN takes
-- the fast path (no rewrite). The one rewrite per table is the generated
-- column below: `ADD COLUMN ... GENERATED ALWAYS AS ... STORED` holds ACCESS
-- EXCLUSIVE for the duration (measured pre-squash: 54.7s for a 149k-row
-- memories plus a 24.8k-row sidecar). The language column has to be added
-- first because the vector reads it — that ordering is why these two blocks
-- are separate statements and not one.
--
-- Sidecar tables need their own column because a generated column cannot read
-- another table — and their vector must tokenise with the same language as the
-- owning memories row, or the base branch and the sidecar branch of one memory
-- stop matching the same queries. The BEFORE INSERT trigger stamps the sidecar
-- from the memories row (which ingest always inserts first, in the same
-- transaction), so the two cannot diverge by construction. The append-only
-- triggers (0010) already reject sidecar UPDATEs, and memories_enforce_immutable
-- below freezes the memories value: the language is decided when the text is
-- written, like the text itself.
-- ---------------------------------------------------------------------------
ALTER TABLE proxima_core.memories
    ADD COLUMN lexical_language regconfig NOT NULL
    DEFAULT proxima_core.lexical_config();

COMMENT ON COLUMN proxima_core.memories.lexical_language IS
'Text-search configuration this row''s text is tokenised with. Stamped at write time (explicit caller choice, reliable detection, or the database default) and immutable afterwards, like the text it describes.';

ALTER TABLE proxima_core.agent_note_v1
    ADD COLUMN lexical_language regconfig NOT NULL
    DEFAULT proxima_core.lexical_config();

ALTER TABLE proxima_core.agent_derivation_v1
    ADD COLUMN lexical_language regconfig NOT NULL
    DEFAULT proxima_core.lexical_config();

CREATE FUNCTION proxima_core.sidecar_lexical_language_from_memory() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE
    memory_language regconfig;
BEGIN
    SELECT m.lexical_language INTO memory_language
      FROM proxima_core.memories m
     WHERE m.memory_id = NEW.memory_id;
    IF memory_language IS NOT NULL THEN
        NEW.lexical_language := memory_language;
    END IF;
    RETURN NEW;
END;
$$;

COMMENT ON FUNCTION proxima_core.sidecar_lexical_language_from_memory() IS
'BEFORE INSERT: stamp the sidecar row''s lexical_language from its owning memories row, so the sidecar''s stored vector tokenises with the same language as the memory''s. Ingest inserts the memories row first in the same transaction; a sidecar inserted without one (test fixtures) keeps its column default.';

CREATE TRIGGER agent_note_v1_lexical_language
    BEFORE INSERT ON proxima_core.agent_note_v1
    FOR EACH ROW EXECUTE FUNCTION proxima_core.sidecar_lexical_language_from_memory();

CREATE TRIGGER agent_derivation_v1_lexical_language
    BEFORE INSERT ON proxima_core.agent_derivation_v1
    FOR EACH ROW EXECUTE FUNCTION proxima_core.sidecar_lexical_language_from_memory();

-- Generated columns are computed after BEFORE triggers, so the sidecar vectors
-- see the stamped language.
ALTER TABLE proxima_core.memories
    ADD COLUMN search_tsv tsvector
    GENERATED ALWAYS AS (
        proxima_core.lexical_tsv(lexical_language, COALESCE(text, ''))) STORED;

ALTER TABLE proxima_core.agent_note_v1
    ADD COLUMN search_tsv tsvector
    GENERATED ALWAYS AS (
        proxima_core.lexical_tsv(lexical_language, proxima_core.lexical_join(
            NULLIF(title, ''),
            NULLIF(body, ''),
            proxima_core.lexical_text_array(tags)))) STORED;

ALTER TABLE proxima_core.agent_derivation_v1
    ADD COLUMN search_tsv tsvector
    GENERATED ALWAYS AS (
        proxima_core.lexical_tsv(lexical_language, proxima_core.lexical_join(
            NULLIF(title, ''),
            NULLIF(body, ''),
            proxima_core.lexical_text_array(tags)))) STORED;

-- The language joins the immutable set (0010): it describes how the frozen
-- text was tokenised, and the sidecar mirror above is written once at
-- insert — a later memories-side UPDATE would silently diverge from the
-- frozen sidecar copy. Same body as 0010 plus the one line.
CREATE OR REPLACE FUNCTION proxima_core.memories_enforce_immutable() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    IF NEW.memory_id IS DISTINCT FROM OLD.memory_id
        OR NEW.schema_id IS DISTINCT FROM OLD.schema_id
        OR NEW.schema_version IS DISTINCT FROM OLD.schema_version
        OR NEW.receipt_id IS DISTINCT FROM OLD.receipt_id
        OR NEW.kind IS DISTINCT FROM OLD.kind
        OR NEW.text IS DISTINCT FROM OLD.text
        OR NEW.operator_kind IS DISTINCT FROM OLD.operator_kind
        OR NEW.operator_id IS DISTINCT FROM OLD.operator_id
        OR NEW.input_contract_id IS DISTINCT FROM OLD.input_contract_id
        OR NEW.source_batch_id IS DISTINCT FROM OLD.source_batch_id
        OR NEW.model_id IS DISTINCT FROM OLD.model_id
        OR NEW.prompt_version IS DISTINCT FROM OLD.prompt_version
        OR NEW.lexical_language IS DISTINCT FROM OLD.lexical_language
    THEN
        RAISE EXCEPTION 'memories append-only: immutable column changed on memory_id=%', OLD.memory_id;
    END IF;
    RETURN NEW;
END;
$$;

-- ---------------------------------------------------------------------------
-- Switching the DEFAULT has to rebuild whatever still binds to it, atomically.
--
-- Redefining `lexical_config()` by hand leaves any column generated through it
-- split-brained (the hazard documented above), so the switch is offered as one
-- operation that does both, and it finds its work by introspection rather than
-- by a hardcoded table list — flavor sidecars generate `search_tsv` from the
-- same function and must be rebuilt too, but core cannot know their names.
--
-- Discovery is by pg_depend and not by a regex over the printed expression: a
-- textual `~ 'lexical_(tsv|config)'` also matches the two-argument spelling
-- `lexical_tsv(lexical_language, …)` — columns whose language comes from the
-- ROW and which a default switch must NOT touch. Rebuilding them would be
-- wasted ACCESS EXCLUSIVE rewrites (not corruption: SET EXPRESSION re-applies
-- the same per-row expression), but a full-table rewrite per table per switch
-- is exactly the cost this function should not impose. A stored generated
-- column is a rebuild target iff its expression depends on `lexical_config()`
-- or the one-argument `lexical_tsv(text)` — the two functions whose VALUE the
-- switch changes. Two-argument columns depend on neither ($$-quoted SQL
-- function bodies record no transitive dependencies), so they are excluded
-- structurally rather than textually.
--
-- Cost is a full table rewrite under ACCESS EXCLUSIVE per affected table, so
-- this is a maintenance-window operation, not a runtime setting.
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

    -- The new default is a language rows will now be written in.
    INSERT INTO proxima_core.lexical_languages (config)
    VALUES (new_config)
    ON CONFLICT (config) DO NOTHING;

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
           AND EXISTS (
               SELECT 1
                 FROM pg_depend dep
                WHERE dep.classid = 'pg_attrdef'::regclass
                  AND dep.objid = d.oid
                  AND dep.refclassid = 'pg_proc'::regclass
                  AND dep.refobjid IN (
                      to_regprocedure('proxima_core.lexical_config()')::oid,
                      to_regprocedure('proxima_core.lexical_tsv(text)')::oid))
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
'Set the DEFAULT lexical text-search configuration: rows written without an explicit or detected language use it from now on. Existing rows keep the language they were stamped with. Stored vectors still generated through the one-argument lexical_tsv/lexical_config (pre-per-row flavor sidecars) are rebuilt in the same transaction, each under an ACCESS EXCLUSIVE table rewrite — returns those columns; per-row columns are untouched.';

-- ---------------------------------------------------------------------------
-- Guarded removal from the active-language set. PostgreSQL does not block
-- DROP TEXT SEARCH CONFIGURATION while table rows hold its regconfig value
-- (no pg_depend entry is recorded for stored values — verified on PG 18.4):
-- the rows are left with a dangling OID that renders as a number and makes
-- any later UPDATE of the row fail with `cache lookup failed`. So the rule
-- is: forget a language here FIRST — this refuses while any row still
-- references it — and only then, if it was a custom configuration, drop it.
-- ---------------------------------------------------------------------------
CREATE FUNCTION proxima_core.lexical_language_forget(config_to_forget regconfig)
RETURNS void
LANGUAGE plpgsql AS
$$
DECLARE
    holder record;
    found  boolean;
    locked boolean;
BEGIN
    IF config_to_forget IS NULL THEN
        RAISE EXCEPTION 'lexical configuration must not be null';
    END IF;
    IF config_to_forget = proxima_core.lexical_config() THEN
        RAISE EXCEPTION 'cannot forget %: it is the default lexical configuration',
            config_to_forget;
    END IF;

    -- Lock the registration row BEFORE scanning. Writers stamping a row in
    -- this language hold FOR KEY SHARE on it until their transaction ends
    -- (see register_lexical_language_in_tx), and FOR UPDATE conflicts with
    -- that — so this blocks until every in-flight write in the language has
    -- committed, and the scan below then sees their rows. Without the
    -- lock, forget is a check-then-delete that a concurrent write slips
    -- past, committing rows in a language that no longer matches anything.
    SELECT true INTO locked
      FROM proxima_core.lexical_languages
     WHERE config = config_to_forget
       FOR UPDATE;
    IF locked IS NOT TRUE THEN
        RETURN; -- not registered: nothing to forget
    END IF;

    FOR holder IN
        SELECT n.nspname AS s, c.relname AS t, a.attname AS col
          FROM pg_attribute a
          JOIN pg_class     c ON c.oid = a.attrelid
          JOIN pg_namespace n ON n.oid = c.relnamespace
         WHERE a.atttypid = 'regconfig'::regtype
           AND a.attnum > 0
           AND NOT a.attisdropped
           -- 'm': a materialized view stores regconfig values as durably as
           -- a table and dangles the same way after a config drop. Foreign
           -- tables ('f') are deliberately NOT scanned — probing one
           -- queries the remote server, which may hang or error; foreign
           -- holders are the operator's own responsibility.
           AND c.relkind IN ('r', 'p', 'm')
           AND NOT (n.nspname = 'proxima_core' AND c.relname = 'lexical_languages')
         ORDER BY n.nspname, c.relname, a.attname
    LOOP
        EXECUTE format('SELECT EXISTS (SELECT 1 FROM %I.%I WHERE %I = %L::regconfig)',
                       holder.s, holder.t, holder.col, config_to_forget::text)
           INTO found;
        IF found THEN
            RAISE EXCEPTION 'cannot forget %: rows in %.% still reference it (column %)',
                config_to_forget, holder.s, holder.t, holder.col;
        END IF;
    END LOOP;

    DELETE FROM proxima_core.lexical_languages
     WHERE config = config_to_forget;
END
$$;

COMMENT ON FUNCTION proxima_core.lexical_language_forget(regconfig) IS
'Remove a configuration from the active-language set, refusing while any table or materialized-view row still holds it in a regconfig column (foreign tables are not scanned). Serialized against in-flight writes via the registration row lock. Run this BEFORE dropping a custom text search configuration: PostgreSQL allows the drop with rows still referencing it, and those rows are then un-updatable (cache lookup failed on the dangling OID).';

-- ---------------------------------------------------------------------------
-- Documents become citable, and citable by page.
--
-- `core/uploaded-blob-v1` has been a registered CitedObject schema since the
-- baseline, and the S3 upload lane has been writing rows into
-- cited_uploaded_blob_v1 the whole time. But no registered
-- CitationMappingPayload named it as its cited_object_schema(), and a
-- mapping is the only path from a Fact to a cited object
-- (memories.citation_mapping_id). `authorize_fact_with_citation` checks that
-- the mapping schema targets the object's schema, so there was no argument a
-- caller could pass that would attach a Fact to an uploaded blob. Core
-- shipped an upload lane whose artefacts nothing could cite.
--
-- Two mappings close it. `core/uploaded-blob-whole-v1` is a pure link and
-- needs no table (see the CitationMappingPayload contract — a fieldless
-- mapping returns None rather than minting an empty table).
-- `core/uploaded-blob-page-span-v1` is the locator docs/11 has always
-- described, and needs the table below.
--
-- Page numbers are one-based and inclusive at both ends: that is how a page
-- is cited in prose and how it is printed on the page. Zero-based would make
-- "page 1" mean the second page in every citation a human reads back.
-- ---------------------------------------------------------------------------

CREATE TABLE proxima_core.citation_uploaded_blob_page_span_v1 (
    citation_mapping_id uuid PRIMARY KEY
        REFERENCES proxima_core.citation_mappings(citation_mapping_id)
        ON DELETE CASCADE,
    page_from integer NOT NULL,
    page_to integer NOT NULL,
    char_range_start integer,
    char_range_end integer,
    CONSTRAINT citation_blob_page_span_pages_chk
        CHECK (page_from >= 1 AND page_to >= page_from),
    -- Both ends or neither: a half-open character range cannot be resolved
    -- back to a substring, and silently treating a missing end as "to the
    -- end of the span" would make two different citations compare equal.
    CONSTRAINT citation_blob_page_span_chars_chk
        CHECK (
            (char_range_start IS NULL) = (char_range_end IS NULL)
            AND (char_range_start IS NULL
                 OR (char_range_start >= 0 AND char_range_end >= char_range_start))
        )
);

COMMENT ON TABLE proxima_core.citation_uploaded_blob_page_span_v1 IS
'Sidecar for core/uploaded-blob-page-span-v1: which pages of a cited uploaded document a Fact came from. Pages are one-based and inclusive at both ends; a single page has page_from = page_to. char_range_* is optional and relative to the text of the span, not of the document. See docs/11-citations.md.';

-- Read pattern: "which Facts cite pages of this document, in page order".
-- The citation_mapping_id primary key answers the reverse direction only.
CREATE INDEX idx_citation_blob_page_span_pages
    ON proxima_core.citation_uploaded_blob_page_span_v1 (page_from, page_to);

-- ---------------------------------------------------------------------------
-- The edge layer is RESET in the spirit of the v0.0.4 reset: the edges table
-- is replaced, not evolved (docs/16-edges.md §Storage Migration). Nothing is
-- carried over. Origin rows come back the moment a node write declares what it
-- was made from; reference rows come back with re-ingest.
-- MIN_CORE_MIGRATION_VERSION is 11, so a database one lane behind the binary
-- fails at boot rather than at first query.
--
-- The thesis this implements: an edge carries no information beyond its
-- existence — its endpoints, its direction, its creation time, and its kind.
-- All content lives in nodes.
-- ---------------------------------------------------------------------------

-- ---------------------------------------------------------------------------
-- Out: the two-layered relation model. Every object dropped here comes from
-- the v0.0.4 baseline (0001_init.sql), which is checksum-pinned and cannot be
-- edited — dropping is the only way to unmake it.
--
-- The old table carried an id, a relation string, a relation class, an
-- authorship kind, an authorship owner, three endpoint columns per side and a
-- typed sidecar. Every one of them was either content that belongs in a node
-- or metadata that belongs on a row. `agent_link_v1` — the one core edge
-- sidecar — held a reason and a confidence, which is a judgment, which is a
-- Perspective: it comes back as `interpretation_v1` below.
-- ---------------------------------------------------------------------------
DROP TABLE IF EXISTS proxima_core.agent_link_v1;
DROP TABLE IF EXISTS proxima_core.compliance_edge_target_redactions;
DROP TABLE IF EXISTS proxima_core.edges;
DROP FUNCTION IF EXISTS proxima_core.validate_edge_invariants();
DROP TYPE IF EXISTS proxima_core.relation_class;
DROP TYPE IF EXISTS proxima_core.edge_authorship_kind;

-- ---------------------------------------------------------------------------
-- In: two kinds, and the enum is not extensible.
--
-- A feature that seems to need a third kind fails the node-home test and is
-- missing a node, not a kind.
-- ---------------------------------------------------------------------------
CREATE TYPE proxima_core.edge_kind AS ENUM (
    'origin',
    'reference'
);

COMMENT ON TYPE proxima_core.edge_kind IS
  'What an edge IS. origin: a node declared what it was made from. reference: a schema-declared payload field points at another node. The kind is a consequence of the write that produced the row, never a choice the writer makes.';

-- The endpoint kind is also the address form, which is also the binding: a
-- FactEntityHead endpoint follows the head as it is re-observed, every other
-- endpoint pins one row. That is where the old descriptor's FollowHead/Pin
-- cell went — into the address itself, so the two cannot disagree.
CREATE TYPE proxima_core.edge_endpoint_kind AS ENUM (
    'Fact',
    'Abstraction',
    'Perspective',
    'Goal',
    'FactEntityHead'
);

COMMENT ON TYPE proxima_core.edge_endpoint_kind IS
  'One end of an edge: the entity kind and, in the same value, the address form. Fact/Abstraction/Perspective address a memories row, Goal a goals row, FactEntityHead a fact_entities row (follow-head). Superset of entity_kind because the address form is part of what the endpoint is.';

CREATE FUNCTION proxima_core.edge_endpoint_layer(kind proxima_core.edge_endpoint_kind)
RETURNS integer
    LANGUAGE sql IMMUTABLE PARALLEL SAFE
    AS $$
    SELECT CASE kind
        WHEN 'Fact'::proxima_core.edge_endpoint_kind THEN 0
        WHEN 'FactEntityHead'::proxima_core.edge_endpoint_kind THEN 0
        WHEN 'Abstraction'::proxima_core.edge_endpoint_kind THEN 1
        WHEN 'Perspective'::proxima_core.edge_endpoint_kind THEN 2
        ELSE NULL
    END;
$$;

COMMENT ON FUNCTION proxima_core.edge_endpoint_layer(proxima_core.edge_endpoint_kind) IS
  'F/A/P layer index; NULL for Goal, which sits outside the layer comparison (docs/16 §Direction and layering).';

CREATE FUNCTION proxima_core.edge_endpoint_entity_kind(kind proxima_core.edge_endpoint_kind)
RETURNS proxima_core.entity_kind
    LANGUAGE sql IMMUTABLE PARALLEL SAFE
    AS $$
    SELECT CASE kind
        WHEN 'FactEntityHead'::proxima_core.edge_endpoint_kind
            THEN 'Fact'::proxima_core.entity_kind
        ELSE kind::text::proxima_core.entity_kind
    END;
$$;

-- ---------------------------------------------------------------------------
-- The edge table is an index.
--
-- No edge_id: rows have no identity beyond their content, so idempotency is
-- structural — replaying any write re-asserts the same primary key. The
-- identity-hash scheme this lane replaced (BLAKE3-derived v8 ids under a
-- NULLS NOT DISTINCT partial unique index) existed to approximate what this
-- table has by construction.
--
-- No payload, no sidecar, no citation, no status. A connection that needs to
-- say more than "these two, this way, since then" is a node.
--
-- Multiplicity collapses: ten call sites from chunk A to chunk B are one row
-- here and ten entries in A's payload.
-- ---------------------------------------------------------------------------
CREATE TABLE proxima_core.edges (
    source_kind proxima_core.edge_endpoint_kind NOT NULL,
    source_id uuid NOT NULL,
    target_kind proxima_core.edge_endpoint_kind NOT NULL,
    target_id uuid NOT NULL,
    kind proxima_core.edge_kind NOT NULL,
    owner_kind proxima_core.owner_ref_kind NOT NULL,
    owner_id uuid,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT edges_pkey PRIMARY KEY (source_kind, source_id, target_kind, target_id, kind),
    CONSTRAINT edges_no_self_loop_chk
        CHECK (NOT (source_kind = target_kind AND source_id = target_id)),
    CONSTRAINT edges_layering_chk CHECK (
        proxima_core.edge_endpoint_layer(source_kind) IS NULL
        OR proxima_core.edge_endpoint_layer(target_kind) IS NULL
        OR proxima_core.edge_endpoint_layer(source_kind)
           >= proxima_core.edge_endpoint_layer(target_kind)
    ),
    CONSTRAINT edges_owner_ref_shape_chk CHECK (
        (owner_kind = 'world'::proxima_core.owner_ref_kind AND owner_id IS NULL)
        OR (owner_kind IN ('personal'::proxima_core.owner_ref_kind,
                           'group'::proxima_core.owner_ref_kind)
            AND owner_id IS NOT NULL)
    ),
    CONSTRAINT edges_world_not_write_owner_chk
        CHECK (owner_kind <> 'world'::proxima_core.owner_ref_kind)
);

COMMENT ON TABLE proxima_core.edges IS
  'The connection index. One row per (source, target, kind); the row IS its identity, so a replayed write re-asserts it instead of minting a duplicate. Owned by the source owner, always. Rebuildable: dropping this table and re-deriving it from node content yields the same set — that is the master invariant, and every other guarantee is a corollary. See docs/16-edges.md.';

COMMENT ON COLUMN proxima_core.edges.kind IS
  'origin (the source declared what it was made from) or reference (a schema-declared payload field of the source points here). Consequent, never chosen.';

CREATE INDEX idx_edges_owner_created ON proxima_core.edges
    USING btree (owner_kind, owner_id, created_at DESC);
CREATE INDEX idx_edges_source ON proxima_core.edges
    USING btree (source_id, source_kind);
CREATE INDEX idx_edges_target ON proxima_core.edges
    USING btree (target_id, target_kind);
CREATE INDEX idx_edges_origin_target ON proxima_core.edges
    USING btree (target_id) WHERE (kind = 'origin'::proxima_core.edge_kind);

-- Resolve one endpoint address to (its actual kind, its owner). No row means
-- the endpoint does not exist, which is how the trigger below spells E1.
CREATE FUNCTION proxima_core.edge_endpoint_row(
    endpoint_kind proxima_core.edge_endpoint_kind,
    endpoint_id uuid
)
RETURNS TABLE (
    actual_kind proxima_core.edge_endpoint_kind,
    owner_kind proxima_core.owner_ref_kind,
    owner_id uuid
)
    LANGUAGE sql STABLE
    AS $$
    SELECT CASE
               WHEN m.kind IS NULL THEN 'Fact'::proxima_core.edge_endpoint_kind
               ELSE m.kind::text::proxima_core.edge_endpoint_kind
           END,
           m.owner_kind,
           m.owner_id
      FROM proxima_core.memories m
     WHERE endpoint_kind <> 'Goal'::proxima_core.edge_endpoint_kind
       AND endpoint_kind <> 'FactEntityHead'::proxima_core.edge_endpoint_kind
       AND m.memory_id = endpoint_id
    UNION ALL
    SELECT 'Goal'::proxima_core.edge_endpoint_kind, g.owner_kind, g.owner_id
      FROM proxima_core.goals g
     WHERE endpoint_kind = 'Goal'::proxima_core.edge_endpoint_kind
       AND g.goal_id = endpoint_id
    UNION ALL
    SELECT 'FactEntityHead'::proxima_core.edge_endpoint_kind, fe.owner_kind, fe.owner_id
      FROM proxima_core.fact_entities fe
     WHERE endpoint_kind = 'FactEntityHead'::proxima_core.edge_endpoint_kind
       AND fe.fact_entity_id = endpoint_id
$$;

-- Existence, ownership and endpoint-kind agreement (Lean E1/E2). Layering
-- (E3) and the self-loop refusal are CHECK constraints above — they read only
-- the row. This trigger is what needs the endpoint tables.
CREATE FUNCTION proxima_core.validate_edge_invariants() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE
    source_actual proxima_core.edge_endpoint_kind;
    source_owner_kind proxima_core.owner_ref_kind;
    source_owner_id uuid;
    target_actual proxima_core.edge_endpoint_kind;
BEGIN
    SELECT actual_kind, owner_kind, owner_id
      INTO source_actual, source_owner_kind, source_owner_id
      FROM proxima_core.edge_endpoint_row(NEW.source_kind, NEW.source_id);
    IF source_actual IS NULL THEN
        RAISE EXCEPTION 'edge: source endpoint not found';
    END IF;
    IF source_actual <> NEW.source_kind THEN
        RAISE EXCEPTION 'edge: source kind % does not match endpoint kind %',
            NEW.source_kind, source_actual;
    END IF;

    SELECT actual_kind INTO target_actual
      FROM proxima_core.edge_endpoint_row(NEW.target_kind, NEW.target_id);
    IF target_actual IS NULL THEN
        RAISE EXCEPTION 'edge: target endpoint not found';
    END IF;
    IF target_actual <> NEW.target_kind THEN
        RAISE EXCEPTION 'edge: target kind % does not match endpoint kind %',
            NEW.target_kind, target_actual;
    END IF;

    IF source_owner_kind <> NEW.owner_kind
       OR source_owner_id IS DISTINCT FROM NEW.owner_id THEN
        RAISE EXCEPTION 'edge: owner is not the source owner';
    END IF;

    RETURN NEW;
END;
$$;

CREATE TRIGGER edges_invariant_check
    BEFORE INSERT OR UPDATE ON proxima_core.edges
    FOR EACH ROW EXECUTE FUNCTION proxima_core.validate_edge_invariants();

-- ---------------------------------------------------------------------------
-- Supersession is a pointer, not a connection.
--
-- It is the same thing persisting through revision, so it lives on the rows:
-- the successor's `supersedes` (already there since the baseline) and the
-- predecessor's `superseded_by`. Both are written in the successor's own
-- transaction, and NO edge row is written for a supersession.
-- ---------------------------------------------------------------------------
ALTER TABLE proxima_core.memories
    ADD COLUMN superseded_by uuid REFERENCES proxima_core.memories(memory_id);

ALTER TABLE proxima_core.memories
    ADD CONSTRAINT memories_superseded_by_not_self_chk
        CHECK (superseded_by IS DISTINCT FROM memory_id);

CREATE UNIQUE INDEX idx_memories_superseded_by_uq
    ON proxima_core.memories USING btree (superseded_by)
    WHERE (superseded_by IS NOT NULL);

COMMENT ON COLUMN proxima_core.memories.superseded_by IS
  'The revision that replaced this row, when one has. The inverse of supersedes, kept on the row so "is this the head?" is a column read rather than an index traversal. Facts are never superseded.';

ALTER TABLE proxima_core.goals
    ADD COLUMN superseded_by uuid REFERENCES proxima_core.goals(goal_id);

ALTER TABLE proxima_core.goals
    ADD CONSTRAINT goals_superseded_by_not_self_chk
        CHECK (superseded_by IS DISTINCT FROM goal_id);

CREATE UNIQUE INDEX idx_goals_superseded_by_uq
    ON proxima_core.goals USING btree (superseded_by)
    WHERE (superseded_by IS NOT NULL);

-- ---------------------------------------------------------------------------
-- Authorship is node metadata.
--
-- "Emitted by Perspective P" is known at write time and answered by the row,
-- so it is a column, not an edge with an authorship mask.
-- ---------------------------------------------------------------------------
ALTER TABLE proxima_core.memories
    ADD COLUMN authoring_perspective_id uuid REFERENCES proxima_core.memories(memory_id);

ALTER TABLE proxima_core.memories
    ADD CONSTRAINT memories_authoring_perspective_not_self_chk
        CHECK (authoring_perspective_id IS DISTINCT FROM memory_id);

CREATE INDEX idx_memories_authoring_perspective
    ON proxima_core.memories USING btree (authoring_perspective_id)
    WHERE (authoring_perspective_id IS NOT NULL);

COMMENT ON COLUMN proxima_core.memories.authoring_perspective_id IS
  'The Perspective that emitted this memory, when one did. Replaces the core/authored edge and its authorship mask: authorship of a node is a property of the node.';

-- ---------------------------------------------------------------------------
-- Goal topology is what the Goal row says it is.
--
-- The Goal knows the Perspective it inspires, the Goals it waits on, and the
-- evidence it rests on. Those three declarations are the home of the
-- statement; the reference rows in `edges` are derived from them, which is
-- what makes the goal side of the index rebuildable (E7).
-- ---------------------------------------------------------------------------
ALTER TABLE proxima_core.goals
    ADD COLUMN assignment_perspective_id uuid REFERENCES proxima_core.memories(memory_id);

ALTER TABLE proxima_core.goals
    ADD COLUMN dependency_goal_ids uuid[] NOT NULL DEFAULT '{}';

ALTER TABLE proxima_core.goals
    ADD COLUMN evidence_memory_ids uuid[] NOT NULL DEFAULT '{}';

COMMENT ON COLUMN proxima_core.goals.assignment_perspective_id IS
  'The self Perspective this Goal inspires (was the core/inspires edge). One reference row is derived from it.';

COMMENT ON COLUMN proxima_core.goals.dependency_goal_ids IS
  'Goals this one waits on (was core/depends-on). One reference row per entry.';

COMMENT ON COLUMN proxima_core.goals.evidence_memory_ids IS
  'Memories this Goal rests on (was core/wake-motivated-by). One reference row per entry.';

-- ---------------------------------------------------------------------------
-- A computed score is an Abstraction, and an Abstraction may cite.
--
-- docs/16 §Computed Scores Are Abstractions amends docs/11 §Multiplicity:
-- citation_mapping_id becomes optional for Fact AND Abstraction. Perspectives
-- still never cite directly — an interpretation grounds through references.
-- Multiplicity stays 0..1 per memory.
-- ---------------------------------------------------------------------------
ALTER TABLE proxima_core.memories
    DROP CONSTRAINT memories_variant_chk;

ALTER TABLE proxima_core.memories
    ADD CONSTRAINT memories_variant_chk CHECK (
        (kind IS NULL
         AND operator_kind IS NULL AND operator_id IS NULL
         AND input_contract_id IS NULL AND source_batch_id IS NULL
         AND model_id IS NULL AND prompt_version IS NULL AND supersedes IS NULL)
        OR (kind IS NOT NULL
            AND text IS NOT NULL
            AND operator_kind IS NOT NULL
            AND operator_id IS NOT NULL
            AND input_contract_id IS NOT NULL
            AND (
                (operator_kind = 'FtoA'::proxima_core.memory_operator_kind
                 AND kind = 'Abstraction'::proxima_core.entity_kind
                 AND source_batch_id IS NOT NULL)
                OR (operator_kind = 'AtoA'::proxima_core.memory_operator_kind
                    AND kind = 'Abstraction'::proxima_core.entity_kind
                    AND source_batch_id IS NULL)
                OR (operator_kind = 'AtoP'::proxima_core.memory_operator_kind
                    AND kind = 'Perspective'::proxima_core.entity_kind
                    AND source_batch_id IS NULL)
            )
            AND model_id IS NOT NULL
            AND prompt_version IS NOT NULL
            AND receipt_id IS NULL
            AND (citation_mapping_id IS NULL
                 OR kind = 'Abstraction'::proxima_core.entity_kind))
    );

ALTER TABLE proxima_core.memories
    ADD CONSTRAINT memories_superseded_by_not_a_fact_chk
        CHECK (superseded_by IS NULL OR kind IS NOT NULL);

COMMENT ON COLUMN proxima_core.memories.citation_mapping_id IS
  'Optional outside-proof for a Fact or an Abstraction (-> citation_mappings). An Abstraction cites the record of the computation that produced it. Forbidden on Perspectives, which ground through their references.';

-- ---------------------------------------------------------------------------
-- The interpretation Perspective.
--
-- core_link stored a reason and a confidence on an edge. A claim with a
-- reason and a confidence is a judgment, and judgments are Perspectives — the
-- edge was a Perspective hiding in a cheaper container. The subjects live in
-- the payload as schema-declared reference fields, so the reference rows that
-- connect an interpretation to what it interprets are re-derivable from this
-- row alone.
-- ---------------------------------------------------------------------------

-- A subject kind is a closed vocabulary, so it is an enum and not text. It is
-- deliberately NOT entity_kind: that enum carries 'Goal', and a Goal is not a
-- memory and cannot be an interpretation subject on this payload. Reusing it
-- would let the column hold a value `InterpretationSubjectKind` cannot
-- represent, which is the widening this type exists to refuse.
CREATE TYPE proxima_core.interpretation_subject_kind AS ENUM (
    'Fact',
    'Abstraction',
    'Perspective'
);

COMMENT ON TYPE proxima_core.interpretation_subject_kind IS
  'Memory layer of an interpretation subject. F/A/P only — a Goal is not a memory and cannot be a subject here. A Perspective may interpret any layer: the layering rule is satisfied because the Perspective, not the subject, is the edge source.';

CREATE TABLE proxima_core.interpretation_v1 (
    memory_id uuid NOT NULL,
    claim text NOT NULL,
    confidence smallint NOT NULL,
    subject_memory_ids uuid[] NOT NULL,
    subject_kinds proxima_core.interpretation_subject_kind[] NOT NULL,
    model_id text NOT NULL,
    client_name text NOT NULL,
    client_version text NOT NULL,
    CONSTRAINT interpretation_v1_pkey PRIMARY KEY (memory_id),
    CONSTRAINT interpretation_v1_memory_id_fkey
        FOREIGN KEY (memory_id) REFERENCES proxima_core.memories(memory_id),
    CONSTRAINT interpretation_v1_claim_nonempty CHECK (length(btrim(claim)) > 0),
    CONSTRAINT interpretation_v1_confidence_range CHECK (confidence BETWEEN 0 AND 100),
    CONSTRAINT interpretation_v1_subjects_aligned
        CHECK (cardinality(subject_memory_ids) = cardinality(subject_kinds))
);

COMMENT ON TABLE proxima_core.interpretation_v1 IS
  'An agent claim about existing nodes (core/interpretation-v1). Successor to the agent_link_v1 edge sidecar: the reason became the claim, the confidence stayed, and the two endpoints became subject references the ingest turns into reference rows.';

CREATE TRIGGER interpretation_v1_append_only BEFORE UPDATE ON proxima_core.interpretation_v1
    FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();

-- ---------------------------------------------------------------------------
-- Compliance: an edge target redaction is keyed by the edge, and the edge is
-- its own key.
-- ---------------------------------------------------------------------------
CREATE TABLE proxima_core.compliance_edge_target_redactions (
    operation_id uuid NOT NULL,
    source_kind proxima_core.edge_endpoint_kind NOT NULL,
    source_id uuid NOT NULL,
    target_kind proxima_core.edge_endpoint_kind NOT NULL,
    target_id uuid NOT NULL,
    kind proxima_core.edge_kind NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT compliance_edge_target_redactions_pkey
        PRIMARY KEY (operation_id, source_kind, source_id, target_kind, target_id, kind),
    CONSTRAINT compliance_edge_target_redactions_operation_id_fkey
        FOREIGN KEY (operation_id) REFERENCES proxima_core.compliance_audit_log(operation_id)
);

CREATE INDEX idx_compliance_edge_target_redactions_edge
    ON proxima_core.compliance_edge_target_redactions
    USING btree (source_kind, source_id, target_kind, target_id, kind);

-- ---------------------------------------------------------------------------
-- change_event carries the edge, not a handle to it.
--
-- The old row carried edge_id + edge_relation + three endpoint columns per
-- side, and the reader hydrated endpoint kinds with a second query. The
-- endpoints are now one (kind, id) pair each, which is the whole edge, so the
-- read is one query and the projection needs nothing it does not already
-- have.
-- ---------------------------------------------------------------------------
ALTER TABLE proxima_core.change_event
    DROP CONSTRAINT change_event_endpoint_chk;

ALTER TABLE proxima_core.change_event
    DROP COLUMN edge_id,
    DROP COLUMN edge_relation,
    DROP COLUMN edge_source_memory_id,
    DROP COLUMN edge_source_goal_id,
    DROP COLUMN edge_source_fact_entity_id,
    DROP COLUMN edge_target_memory_id,
    DROP COLUMN edge_target_goal_id,
    DROP COLUMN edge_target_fact_entity_id;

ALTER TABLE proxima_core.change_event
    ADD COLUMN edge_kind proxima_core.edge_kind,
    ADD COLUMN edge_source_kind proxima_core.edge_endpoint_kind,
    ADD COLUMN edge_source_id uuid,
    ADD COLUMN edge_target_kind proxima_core.edge_endpoint_kind,
    ADD COLUMN edge_target_id uuid;

ALTER TABLE proxima_core.change_event
    ADD CONSTRAINT change_event_endpoint_chk CHECK (
        CASE
            WHEN kind IN ('EdgeAppend', 'EdgeDelete') THEN
                entity_kind IS NULL
                AND entity_memory_id IS NULL AND entity_goal_id IS NULL
                AND entity_schema_id IS NULL AND entity_schema_version IS NULL
                AND supersedes_memory_id IS NULL AND supersedes_goal_id IS NULL
                AND edge_kind IS NOT NULL
                AND edge_source_kind IS NOT NULL AND edge_source_id IS NOT NULL
                AND edge_target_kind IS NOT NULL AND edge_target_id IS NOT NULL
            ELSE
                num_nonnulls(entity_memory_id, entity_goal_id) = 1
                AND entity_kind IS NOT NULL
                AND entity_schema_id IS NOT NULL
                AND entity_schema_version IS NOT NULL
                AND edge_kind IS NULL
                AND edge_source_kind IS NULL AND edge_source_id IS NULL
                AND edge_target_kind IS NULL AND edge_target_id IS NULL
                AND NOT (supersedes_memory_id IS NOT NULL AND supersedes_goal_id IS NOT NULL)
        END
    );

COMMENT ON CONSTRAINT change_event_endpoint_chk ON proxima_core.change_event IS
  'Guards the pull-read decode (change_event.rs). EdgeAppend/EdgeDelete rows carry the whole edge — source (kind, id), target (kind, id), edge kind — and no entity/supersedes columns. EntityAppend/EntityDelete rows carry exactly one of entity_memory_id/entity_goal_id plus entity_kind/schema, at most one supersedes endpoint, and no edge columns. Keeps a raw INSERT from persisting an undecodable row.';
