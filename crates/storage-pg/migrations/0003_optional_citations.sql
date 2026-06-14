-- Citations are optional on Facts as of 2026-06-13.
--
-- A Fact (event_id IS NOT NULL) no longer requires citation_mapping_id IS NOT NULL.
-- This forward migration replaces an earlier in-place edit of the squashed v0.0.1 init
-- (0001_init.sql): editing an already-applied migration changes its sqlx checksum and
-- breaks run_migrations() on any database that adopted the squashed init. 0001_init.sql is
-- restored to its original bytes; the constraint relaxation lives here instead.
--
-- End state is identical to the previous in-place edit. Kernel/contract: see the retirement
-- of CI-1a (fact_has_citation) in docs/lean/Foundations/Citations.lean.

ALTER TABLE proxima_core.memories DROP CONSTRAINT memories_variant_chk;

ALTER TABLE proxima_core.memories
    ADD CONSTRAINT memories_variant_chk CHECK (
        (
            (event_id IS NOT NULL)
            AND (kind IS NULL)
            AND (text IS NULL)
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
