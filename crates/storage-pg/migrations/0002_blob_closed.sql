-- Slice 3: blob + closed_handle. Pins are memory.origins/refs (already GIN).

CREATE TABLE proxima_core.blob (
    blob_id uuid PRIMARY KEY DEFAULT uuidv7(),
    owner_id uuid NOT NULL REFERENCES proxima_core.owners (owner_id),
    schema_id text NOT NULL,
    content_hash bytea NOT NULL,
    UNIQUE (owner_id, schema_id, content_hash),
    CONSTRAINT blob_hash_len_chk CHECK (octet_length(content_hash) = 32)
);

CREATE TABLE proxima_core.closed_handle (
    handle uuid PRIMARY KEY,
    closed_at timestamptz NOT NULL DEFAULT now()
);

ALTER TABLE proxima_core.memory
    ADD CONSTRAINT memory_blob_fk
    FOREIGN KEY (blob_id) REFERENCES proxima_core.blob (blob_id);

CREATE FUNCTION proxima_core.memory_pin_checks() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE
    pin uuid;
    pin_kind proxima_core.memory_kind;
    pin_handle uuid;
BEGIN
    FOREACH pin IN ARRAY NEW.origins || NEW.refs LOOP
        SELECT kind, handle INTO pin_kind, pin_handle
          FROM proxima_core.memory
         WHERE t = pin;
        IF NOT FOUND THEN
            RAISE EXCEPTION 'pin % does not exist', pin USING ERRCODE = '23503';
        END IF;
        IF EXISTS (SELECT 1 FROM proxima_core.closed_handle WHERE handle = pin_handle) THEN
            RAISE EXCEPTION 'closed_handle: no new pin to %', pin_handle USING ERRCODE = '23514';
        END IF;
    END LOOP;

    IF NEW.kind = 'abstraction' THEN
        FOREACH pin IN ARRAY NEW.origins LOOP
            SELECT kind INTO pin_kind FROM proxima_core.memory WHERE t = pin;
            IF pin_kind IS DISTINCT FROM 'fact' THEN
                RAISE EXCEPTION 'abstraction origins must be fact t' USING ERRCODE = '23514';
            END IF;
        END LOOP;
    ELSIF NEW.kind = 'perspective' AND NEW.origins <> '{}' THEN
        FOREACH pin IN ARRAY NEW.origins LOOP
            SELECT kind INTO pin_kind FROM proxima_core.memory WHERE t = pin;
            IF pin_kind IS DISTINCT FROM 'abstraction' THEN
                RAISE EXCEPTION 'perspective origins must be abstraction t' USING ERRCODE = '23514';
            END IF;
        END LOOP;
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER memory_pin_checks
    BEFORE INSERT ON proxima_core.memory
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.memory_pin_checks();
