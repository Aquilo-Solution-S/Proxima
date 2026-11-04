-- Retention: how long one owner keeps one schema's memory, one row per owner
-- and schema (docs/data-and-erasure.md). An absent row keeps for good.
-- Owner export carries the rows and owner erase takes them, as declared on
-- the kernel surface. Immutable once released.

CREATE TABLE proxiima_core.retention (
    owner_id uuid NOT NULL REFERENCES proxiima_core.owners(owner_id),
    schema_id text NOT NULL,
    keep_days integer NOT NULL,
    CONSTRAINT retention_pkey PRIMARY KEY (owner_id, schema_id),
    CONSTRAINT retention_keep_days_chk CHECK (keep_days BETWEEN 1 AND 36500)
);

ALTER TABLE proxiima_core.retention ENABLE ROW LEVEL SECURITY;
ALTER TABLE ONLY proxiima_core.retention FORCE ROW LEVEL SECURITY;
