-- Proxima core schema — v0.0.8 timeseries baseline (fresh CREATE).
-- No ALTER of 0001–0021. Existing databases must reset.

CREATE EXTENSION IF NOT EXISTS vector;

CREATE SCHEMA proxima_core;

CREATE TYPE proxima_core.owner_kind AS ENUM (
    'world',
    'personal',
    'group'
);

CREATE TYPE proxima_core.memory_kind AS ENUM (
    'fact',
    'abstraction',
    'perspective'
);

CREATE TYPE proxima_core.announce_op AS ENUM (
    'append',
    'forget',
    'erase'
);

CREATE TYPE proxima_core.announce_entity AS ENUM (
    'memory',
    'goal'
);

CREATE TABLE proxima_core.owners (
    owner_id uuid PRIMARY KEY,
    kind proxima_core.owner_kind NOT NULL,
    CONSTRAINT owners_world_kind_chk CHECK (
        (kind = 'world') = (owner_id = '00000000-0000-0000-0000-000000000001'::uuid)
    )
);

INSERT INTO proxima_core.owners (owner_id, kind)
VALUES ('00000000-0000-0000-0000-000000000001'::uuid, 'world');

CREATE INDEX owners_kind_idx
    ON proxima_core.owners (kind, owner_id);

CREATE TABLE proxima_core.memory_head (
    handle uuid PRIMARY KEY,
    kind proxima_core.memory_kind NOT NULL,
    schema_id text NOT NULL,
    owner_id uuid NOT NULL REFERENCES proxima_core.owners (owner_id),
    t uuid NOT NULL
);

CREATE INDEX memory_head_owner_schema_idx
    ON proxima_core.memory_head (owner_id, schema_id, handle);

CREATE INDEX memory_head_owner_kind_idx
    ON proxima_core.memory_head (owner_id, kind, handle);

CREATE TABLE proxima_core.memory (
    handle uuid NOT NULL REFERENCES proxima_core.memory_head (handle),
    t uuid NOT NULL DEFAULT uuidv7(),
    kind proxima_core.memory_kind NOT NULL,
    owner_id uuid NOT NULL REFERENCES proxima_core.owners (owner_id),
    source_id text,
    ingest_key text,
    blob_id uuid,
    origins uuid[] NOT NULL DEFAULT '{}',
    refs uuid[] NOT NULL DEFAULT '{}',
    PRIMARY KEY (handle, t),
    UNIQUE (t),
    CONSTRAINT memory_fact_source_chk CHECK (
        (kind = 'fact' AND (source_id IS NULL) = (ingest_key IS NULL))
        OR (kind <> 'fact' AND source_id IS NULL AND ingest_key IS NULL)
    ),
    CONSTRAINT memory_fact_origins_chk CHECK (
        kind <> 'fact' OR origins = '{}'
    ),
    CONSTRAINT memory_blob_fa_chk CHECK (
        blob_id IS NULL OR kind IN ('fact', 'abstraction')
    ),
    CONSTRAINT memory_origins_no_null_chk CHECK (array_position(origins, NULL) IS NULL),
    CONSTRAINT memory_refs_no_null_chk CHECK (array_position(refs, NULL) IS NULL)
);

CREATE INDEX memory_owner_handle_t_idx
    ON proxima_core.memory (owner_id, handle, t DESC);

CREATE INDEX memory_owner_t_handle_idx
    ON proxima_core.memory (owner_id, t, handle);

CREATE INDEX memory_origins_gin
    ON proxima_core.memory USING gin (origins);

CREATE INDEX memory_refs_gin
    ON proxima_core.memory USING gin (refs);

CREATE TABLE proxima_core.ingest_keys (
    owner_id uuid NOT NULL REFERENCES proxima_core.owners (owner_id),
    source_id text NOT NULL,
    ingest_key text NOT NULL,
    t uuid NOT NULL,
    PRIMARY KEY (owner_id, source_id, ingest_key)
);

CREATE TABLE proxima_core.announce (
    seq uuid PRIMARY KEY DEFAULT uuidv7(),
    owner_id uuid NOT NULL REFERENCES proxima_core.owners (owner_id),
    op proxima_core.announce_op NOT NULL,
    entity proxima_core.announce_entity NOT NULL,
    handle uuid NOT NULL,
    t uuid NOT NULL
);

CREATE INDEX announce_owner_seq_idx
    ON proxima_core.announce (owner_id, seq);

CREATE FUNCTION proxima_core.enforce_row_append_only() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    RAISE EXCEPTION 'append-only: % does not accept UPDATE', TG_TABLE_NAME
        USING ERRCODE = '25006';
END;
$$;

CREATE FUNCTION proxima_core.memory_align_head() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE
    head_kind proxima_core.memory_kind;
    head_owner uuid;
BEGIN
    SELECT kind, owner_id INTO head_kind, head_owner
      FROM proxima_core.memory_head
     WHERE handle = NEW.handle;
    IF head_kind IS DISTINCT FROM NEW.kind OR head_owner IS DISTINCT FROM NEW.owner_id THEN
        RAISE EXCEPTION 'memory kind/owner must equal memory_head'
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;

CREATE FUNCTION proxima_core.memory_head_t_only() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    IF NEW.handle IS DISTINCT FROM OLD.handle
       OR NEW.kind IS DISTINCT FROM OLD.kind
       OR NEW.schema_id IS DISTINCT FROM OLD.schema_id
       OR NEW.owner_id IS DISTINCT FROM OLD.owner_id THEN
        RAISE EXCEPTION 'memory_head is frozen except t'
            USING ERRCODE = '25006';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER memory_append_only
    BEFORE UPDATE ON proxima_core.memory
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.enforce_row_append_only();

CREATE TRIGGER ingest_keys_append_only
    BEFORE UPDATE ON proxima_core.ingest_keys
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.enforce_row_append_only();

CREATE TRIGGER announce_append_only
    BEFORE UPDATE ON proxima_core.announce
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.enforce_row_append_only();

CREATE TRIGGER owners_append_only
    BEFORE UPDATE ON proxima_core.owners
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.enforce_row_append_only();

CREATE TRIGGER memory_align_head
    BEFORE INSERT ON proxima_core.memory
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.memory_align_head();

CREATE TRIGGER memory_head_t_only
    BEFORE UPDATE ON proxima_core.memory_head
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.memory_head_t_only();
