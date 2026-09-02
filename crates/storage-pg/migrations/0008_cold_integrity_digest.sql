-- Additive over the upload-content lane.
--
-- The cold object is durable outside PostgreSQL. A sidecar stamp inside that
-- object can describe itself, but it cannot prove that the bytes are the
-- bytes written for this cooled admission. Keep the exact encoded-object
-- BLAKE3 digest beside the locator so hydration can fail closed on a changed
-- provider object. Existing rows remain NULL: they have no witness and must
-- be surfaced as unsupported until an operator independently repairs them.

ALTER TABLE proxima_core.cooled
    ADD COLUMN cold_digest bytea,
    ADD CONSTRAINT cooled_cold_digest_len_chk
        CHECK (cold_digest IS NULL OR octet_length(cold_digest) = 32);

-- The digest is part of the cooled witness. Only the existing transfer remap
-- columns remain mutable; otherwise an arbitrary UPDATE could replace the
-- database evidence after the object was read and before hydration commits.
CREATE OR REPLACE FUNCTION proxima_core.cooled_append_only()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.t IS DISTINCT FROM OLD.t
       OR NEW.handle IS DISTINCT FROM OLD.handle
       OR NEW.kind IS DISTINCT FROM OLD.kind
       OR NEW.object_key IS DISTINCT FROM OLD.object_key
       OR NEW.source_id IS DISTINCT FROM OLD.source_id
       OR NEW.ingest_key IS DISTINCT FROM OLD.ingest_key
       OR NEW.origins IS DISTINCT FROM OLD.origins
       OR NEW.refs IS DISTINCT FROM OLD.refs
       OR NEW.goal_refs IS DISTINCT FROM OLD.goal_refs
       OR NEW.cooled_at IS DISTINCT FROM OLD.cooled_at
       OR NEW.cold_digest IS DISTINCT FROM OLD.cold_digest
    THEN
        RAISE EXCEPTION
            'cooled is frozen except owner_id, blob_id and content_id remaps'
            USING ERRCODE = '25006';
    END IF;
    RETURN NEW;
END;
$$;
