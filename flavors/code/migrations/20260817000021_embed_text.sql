-- Store the drain embed string at write. Core sidecars generate the
-- projection concat; chunks generate render_code_slice (path:start-end
-- plus body), not file_path + text.

ALTER TABLE proxima_code.code_chunk_v1
    ADD COLUMN embed_text text
    GENERATED ALWAYS AS (
        NULLIF(
            CASE state
                WHEN 'Present' THEN
                    file_path
                    || ':'
                    || line_range_start::text
                    || '-'
                    || line_range_end::text
                    || E'\n'
                    || text
                ELSE
                    '(deleted slice) '
                    || file_path
                    || '#'
                    || chunk_index::text
            END,
            ''
        )
    ) STORED;

ALTER TABLE proxima_code.file_revision_v1
    ADD COLUMN embed_text text
    GENERATED ALWAYS AS (
        proxima_core.lexical_join(
            VARIADIC ARRAY[
                NULLIF(file_path, ''),
                NULLIF(language, ''),
                NULLIF(indexed_commit_sha, '')
            ]
        )
    ) STORED;

ALTER TABLE proxima_code.commit_v1
    ADD COLUMN embed_text text
    GENERATED ALWAYS AS (
        proxima_core.lexical_join(
            VARIADIC ARRAY[
                NULLIF(sha, ''),
                NULLIF(message, ''),
                NULLIF(author_name, ''),
                NULLIF(author_email, '')
            ]
        )
    ) STORED;

ALTER TABLE proxima_code.commit_summary_v1
    ADD COLUMN embed_text text
    GENERATED ALWAYS AS (
        proxima_core.lexical_join(
            VARIADIC ARRAY[
                NULLIF(commit_sha, ''),
                NULLIF(summary, ''),
                proxima_core.lexical_text_array(key_files)
            ]
        )
    ) STORED;
