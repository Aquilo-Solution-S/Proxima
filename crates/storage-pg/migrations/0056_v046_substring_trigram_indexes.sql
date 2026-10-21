-- The substring fallback matches `LIKE '%q%'` against a sidecar's search text.
-- It runs for every schema the full-text arm found nothing in, and without an
-- index it read every row of the owner's memories of that schema. A trigram
-- index on the exact expression the arm matches serves the pattern, so a
-- search that misses reads the index and not the table.
--
-- Building each index holds a lock that blocks writes to its table until it
-- finishes. `pg_trgm` is a required extension; this creates it as the baseline
-- creates `vector` and `btree_gin`.
CREATE EXTENSION IF NOT EXISTS pg_trgm;

CREATE INDEX agent_derivation_v1_substring_trgm
    ON proxima_core.agent_derivation_v1 USING gin
    ((lower(proxima_core.lexical_join(VARIADIC ARRAY[NULLIF(title, ''), NULLIF(body, ''), proxima_core.lexical_text_array(tags)]))) public.gin_trgm_ops);

CREATE INDEX agent_note_v1_substring_trgm
    ON proxima_core.agent_note_v1 USING gin
    ((lower(proxima_core.lexical_join(VARIADIC ARRAY[NULLIF(title, ''), NULLIF(body, ''), proxima_core.lexical_text_array(tags)]))) public.gin_trgm_ops);

CREATE INDEX interpretation_v1_substring_trgm
    ON proxima_core.interpretation_v1 USING gin
    ((lower(proxima_core.lexical_join(VARIADIC ARRAY[NULLIF(claim, '')]))) public.gin_trgm_ops);

CREATE INDEX utterance_v1_substring_trgm
    ON proxima_core.utterance_v1 USING gin
    ((lower(proxima_core.lexical_join(VARIADIC ARRAY[NULLIF(text, '')]))) public.gin_trgm_ops);
