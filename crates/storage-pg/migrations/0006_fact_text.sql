-- Facts store typed render() text for lexical search.
--
-- 0003_optional_citations relaxed citation_mapping_id on Facts. This
-- migration keeps that shape and only allows `text` on the Fact arm so
-- typed payload render text can live in proxima_core.memories.text.

ALTER TABLE proxima_core.memories DROP CONSTRAINT memories_variant_chk;

ALTER TABLE proxima_core.memories
    ADD CONSTRAINT memories_variant_chk CHECK (
        (
            (event_id IS NOT NULL)
            AND (kind IS NULL)
            AND (operator_kind IS NULL)
            AND (model_id IS NULL)
            AND (prompt_version IS NULL)
            AND (supersedes IS NULL)
        )
        OR (
            (kind IS NOT NULL)
            AND (text IS NOT NULL)
            AND (operator_kind IS NOT NULL)
            AND (model_id IS NOT NULL)
            AND (prompt_version IS NOT NULL)
            AND (event_id IS NULL)
            AND (citation_mapping_id IS NULL)
        )
    );
