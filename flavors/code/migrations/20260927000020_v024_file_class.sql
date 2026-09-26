-- v0.0.24: the file class a code chunk was cut from (#360).
--
-- Ingest classifies every file as source, generated, vendored or lockfile
-- (`flavors/code/src/file_class.rs`) and stamps the class on each chunk.
-- Search filters on it and ranks every source match above every other one.
--
-- `file_class` is nullable on purpose. A chunk written before this file
-- carries none, and neither does a cold dump taken before it: cold hydrate
-- refuses a dump that lacks a NOT NULL column. Readers take NULL as
-- 'source', which is what every chunk was treated as until now.
--
-- `embed_text` is dropped and re-added so a lockfile chunk embeds only its
-- header, `(lockfile) path:start-end`. The chunk keeps a vector — embedding
-- coverage and reconcile count every head as embeddable — but pinned
-- dependency versions no longer sit in the same neighbourhood as code.
-- Every other class embeds exactly what it did. Nothing depends on the
-- column. Re-adding a STORED generated column rewrites the table under an
-- ACCESS EXCLUSIVE lock, once, at upgrade.

CREATE TYPE proxima_code.file_class AS ENUM ('source', 'generated', 'vendored', 'lockfile');

COMMENT ON TYPE proxima_code.file_class IS 'What kind of file a code chunk was cut from. Assigned at ingest from lockfile names, .gitattributes linguist-generated/linguist-vendored/-diff, built-in generated and vendored paths, and the "Code generated ... DO NOT EDIT." header.';

ALTER TABLE proxima_code.code_chunk_v1
    ADD COLUMN file_class proxima_code.file_class;

COMMENT ON COLUMN proxima_code.code_chunk_v1.file_class IS 'Class of the file this chunk was cut from. NULL on chunks written before v0.0.24; read as source.';

ALTER TABLE proxima_code.code_chunk_v1 DROP COLUMN embed_text;

ALTER TABLE proxima_code.code_chunk_v1
    ADD COLUMN embed_text text GENERATED ALWAYS AS (
        NULLIF(
            CASE
                WHEN state <> 'Present' THEN
                    '(deleted slice) ' || file_path || '#' || chunk_index::text
                WHEN file_class = 'lockfile' THEN
                    '(lockfile) ' || file_path || ':' || line_range_start::text || '-' || line_range_end::text
                ELSE
                    file_path || ':' || line_range_start::text || '-' || line_range_end::text || E'\n' || text
            END,
            ''
        )
    ) STORED;
