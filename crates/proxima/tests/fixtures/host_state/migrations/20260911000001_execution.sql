CREATE SCHEMA IF NOT EXISTS host_fixture;

CREATE TABLE host_fixture.execution (
    invocation_id uuid PRIMARY KEY,
    owner_kind proxima_core.owner_kind NOT NULL,
    owner_id uuid NOT NULL,
    status text NOT NULL,
    version integer NOT NULL,
    CONSTRAINT execution_status_check CHECK (status IN ('created', 'finalized'))
);

CREATE INDEX execution_by_owner ON host_fixture.execution (owner_kind, owner_id);
