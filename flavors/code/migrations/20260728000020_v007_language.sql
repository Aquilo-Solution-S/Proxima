-- Code flavor — v0.0.7 lane.
--
-- Pin code chunks to the `english` lexical configuration, per row.
--
-- Core migration 0014 makes the lexical language a property of the row and
-- demotes `lexical_config()` to a default. Before this migration, chunk
-- vectors followed that database default — so a deployment that switches its
-- documents to `german` (the whole point of the setting) would retokenise
-- every code chunk with a German stemmer as collateral. Code is not prose in
-- the deployment's language: identifiers, keywords, and comments are
-- English-dominant, and 20260726000020 measured `english` as the difference
-- between 0/24 answerable natural-language queries and a working search.
--
-- So chunks pin `english` per row instead of following the default. No
-- mirror trigger from the owning memories row here — the right language for
-- a chunk is a property of what a chunk IS, not of what the deployment's
-- documents are written in. `search_chunks`' query side pins the same
-- constant (flavors/code/src/mcp/search_chunks.rs), and core's
-- `core_search_memories` reads the language per row through
-- `CodeChunkV1::search_projection().language_column`, so all three surfaces
-- agree by construction.
--
-- ADD COLUMN with a constant default is metadata-only; the SET EXPRESSION
-- rewrites the table under ACCESS EXCLUSIVE (proportional to indexed corpus
-- size — see MIGRATING.md's v0.0.7 lane). Values are unchanged where the
-- database default was still `english`; where it was not, this rewrite is
-- exactly the repair that restores English tokenisation to code.

ALTER TABLE proxima_code.code_chunk_v1
    ADD COLUMN lexical_language regconfig NOT NULL
    DEFAULT 'english'::regconfig;

COMMENT ON COLUMN proxima_code.code_chunk_v1.lexical_language IS
'Text-search configuration for this chunk''s stored vector. Pinned english per row: code search must not follow proxima_core.set_lexical_config, which serves the deployment''s prose.';

ALTER TABLE proxima_code.code_chunk_v1
    ALTER COLUMN search_tsv SET EXPRESSION AS (
        proxima_core.lexical_tsv(lexical_language, proxima_core.lexical_join(
            NULLIF(file_path, ''),
            NULLIF(text, '')))
    );

COMMENT ON COLUMN proxima_code.code_chunk_v1.search_tsv IS
'Lexical vector over file_path + text via the two-argument proxima_core.lexical_tsv under the row''s lexical_language (pinned english), so CodeChunkV1::search_projection() can name this column as its tsv_column. Must stay identical to lexical_tsv(lexical_language, lexical_join(<projected fields>)).';
