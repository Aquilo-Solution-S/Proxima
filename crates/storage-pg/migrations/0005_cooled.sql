-- Slice 6: cooled stub after forget. ingest_keys is untouched.

CREATE TABLE proxima_core.cooled (
    t uuid PRIMARY KEY,
    handle uuid NOT NULL,
    owner_id uuid NOT NULL REFERENCES proxima_core.owners (owner_id),
    kind proxima_core.memory_kind NOT NULL,
    object_key text NOT NULL,
    cooled_at timestamptz NOT NULL DEFAULT now()
);
