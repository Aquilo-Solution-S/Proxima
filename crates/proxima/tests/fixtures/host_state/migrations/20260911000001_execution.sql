CREATE SCHEMA IF NOT EXISTS host_fixture;

CREATE TABLE host_fixture.execution (
    invocation_id uuid PRIMARY KEY,
    owner_kind proxima_core.owner_kind NOT NULL,
    owner_id uuid NOT NULL,
    status text NOT NULL,
    version integer NOT NULL,
    principal_kind proxima_core.owner_kind,
    principal_id uuid,
    maintenance_origin boolean NOT NULL,
    payload_saved_by_kind proxima_core.owner_kind,
    payload_saved_by_id uuid,
    CONSTRAINT execution_status_check CHECK (status IN ('created', 'finalized')),
    CONSTRAINT execution_origin_shape CHECK (
        (maintenance_origin AND principal_kind IS NULL AND principal_id IS NULL)
        OR
        (NOT maintenance_origin AND principal_kind IS NOT NULL AND principal_id IS NOT NULL)
    ),
    CONSTRAINT execution_payload_saved_by_pair CHECK (
        (payload_saved_by_kind IS NULL) = (payload_saved_by_id IS NULL)
    )
);

CREATE INDEX execution_by_owner ON host_fixture.execution (owner_kind, owner_id);

CREATE TABLE host_fixture.auxiliary (
    invocation_id uuid PRIMARY KEY,
    owner_kind proxima_core.owner_kind NOT NULL,
    owner_id uuid NOT NULL
);

CREATE TABLE host_fixture.deferred_reference (
    invocation_id uuid PRIMARY KEY,
    execution_id uuid NOT NULL,
    CONSTRAINT deferred_reference_execution_fk
        FOREIGN KEY (execution_id) REFERENCES host_fixture.execution(invocation_id)
        DEFERRABLE INITIALLY DEFERRED
);
