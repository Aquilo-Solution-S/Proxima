-- Proxima core schema — v0.0.7 append-only migration (version 14).
--
-- 0001_init.sql is the SHIPPED v0.0.4 baseline (sqlx checksum-pinned, NEVER
-- edit). 0008..0013 are the prior append-only lanes.

-- ---------------------------------------------------------------------------
-- The lexical language becomes a property of the ROW; the database setting
-- (0012's `lexical_config()`) becomes the default for rows that do not say.
--
-- 0012 made the text-search configuration one database-wide value. That is
-- the wrong shape the moment a corpus mixes languages, which real corpora do:
-- a German handbook and an English design doc in one deployment cannot share
-- a stemmer. Measured on 2,350 pages of German technical literature with 130
-- verified questions, the wrong configuration answers 1 in 15 of the
-- questions that do not reuse the source page's wording; the right one
-- answers 1 in 4.
--
-- Making the language a `regconfig` column works because the two-argument
-- `to_tsvector(regconfig, text)` is IMMUTABLE, so a stored generated column
-- may read a sibling column for its configuration (verified on PG 18.4). The
-- column must be TYPE regconfig: a text column with a `::regconfig` cast is
-- rejected — the cast resolves names through the search_path and is only
-- STABLE.
--
-- This also retires a hazard by construction. 0012's switch machinery exists
-- because redefining `lexical_config()` does not recompute dependent stored
-- columns (the split-brain 0012 documents). With the language as a column
-- VALUE, PostgreSQL sees the dependency: the vector is recomputed from the
-- row itself, and there is no function redefinition to go stale against.
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

-- The two-argument vector: genuinely immutable — nothing in it reads mutable
-- state. The one-argument form (0011/0012) stays as the default-language
-- spelling for callers without a language column and as the boot marker.
CREATE FUNCTION proxima_core.lexical_tsv(config regconfig, txt text) RETURNS tsvector
LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE AS
$$ SELECT to_tsvector(config, proxima_core.lexical_scrub(txt)) $$;

COMMENT ON FUNCTION proxima_core.lexical_tsv(regconfig, text) IS
'The canonical lexical vector for one text blob under an explicit text-search configuration. Stored search_tsv columns generate through this with the row''s lexical_language, so the language a text was tokenised with is data on the row, not a function definition that can drift.';

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
-- Per-row language columns. `DEFAULT proxima_core.lexical_config()` keeps
-- 0012's setting as exactly what it now is: the default. The default is
-- non-volatile, so ADD COLUMN takes the fast path (no rewrite); the one
-- rewrite per table is the SET EXPRESSION below.
--
-- Sidecar tables need their own column because a generated column cannot
-- read another table — and their vector must tokenise with the same language
-- as the owning memories row, or the base branch and the sidecar branch of
-- one memory stop matching the same queries. The BEFORE INSERT trigger
-- stamps the sidecar from the memories row (which ingest always inserts
-- first, in the same transaction), so the two cannot diverge by construction.
-- The append-only triggers (0010) already reject sidecar UPDATEs, and
-- memories_enforce_immutable below freezes the memories value: the language
-- is decided when the text is written, like the text itself.
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

-- Move the three stored vectors to the two-argument form. Generated columns
-- are computed after BEFORE triggers, so the sidecar vectors see the stamped
-- language. Each SET EXPRESSION rewrites its table under ACCESS EXCLUSIVE
-- (0011 measured 54.7s for 149k memories plus a 24.8k-row sidecar) — the
-- values are unchanged (every existing row's language IS the default), the
-- rewrite is the cost of rebinding the expression.
ALTER TABLE proxima_core.memories
    ALTER COLUMN search_tsv SET EXPRESSION AS
    (proxima_core.lexical_tsv(lexical_language, COALESCE(text, '')));

ALTER TABLE proxima_core.agent_note_v1
    ALTER COLUMN search_tsv SET EXPRESSION AS
    (proxima_core.lexical_tsv(lexical_language, proxima_core.lexical_join(
        NULLIF(title, ''),
        NULLIF(body, ''),
        proxima_core.lexical_text_array(tags))));

ALTER TABLE proxima_core.agent_derivation_v1
    ALTER COLUMN search_tsv SET EXPRESSION AS
    (proxima_core.lexical_tsv(lexical_language, proxima_core.lexical_join(
        NULLIF(title, ''),
        NULLIF(body, ''),
        proxima_core.lexical_text_array(tags))));

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
-- set_lexical_config now sets the DEFAULT and rebuilds only what still binds
-- to it.
--
-- 0012 discovered rebuild targets by regex over the printed expression
-- (`~ 'lexical_(tsv|config)'`). That regex also matches the two-argument
-- spelling `lexical_tsv(lexical_language, …)` — columns whose language comes
-- from the ROW and which a default switch must NOT touch. Rebuilding them
-- would be wasted ACCESS EXCLUSIVE rewrites (not corruption: SET EXPRESSION
-- re-applies the same per-row expression), but a full-table rewrite per
-- table per switch is exactly the cost this function should not impose.
--
-- Discovery therefore moves to pg_depend: a stored generated column is a
-- rebuild target iff its expression depends on `lexical_config()` or the
-- one-argument `lexical_tsv(text)` — the two functions whose VALUE the
-- switch changes. Two-argument columns depend on neither ($$-quoted SQL
-- function bodies record no transitive dependencies), so they are excluded
-- structurally rather than textually. Flavor sidecars still on the
-- one-argument form keep exactly the 0012 behavior until their own
-- migration moves them.
-- ---------------------------------------------------------------------------
CREATE OR REPLACE FUNCTION proxima_core.set_lexical_config(new_config regconfig)
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

COMMENT ON FUNCTION proxima_core.lexical_config() IS
'The DEFAULT text-search configuration: used for rows written without an explicit or detected language, and as the query-side fallback when lexical_languages is empty. Since v14 the per-row memories.lexical_language is authoritative for each stored vector. Change only through proxima_core.set_lexical_config().';

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
