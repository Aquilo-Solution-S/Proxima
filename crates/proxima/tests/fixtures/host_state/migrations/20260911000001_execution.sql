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

ALTER TABLE host_fixture.execution ENABLE ROW LEVEL SECURITY;
ALTER TABLE host_fixture.execution FORCE ROW LEVEL SECURITY;
CREATE POLICY proxima_owner_read ON host_fixture.execution
    FOR SELECT TO PUBLIC
    USING (owner_id = ANY((SELECT COALESCE(NULLIF(current_setting('app.owner', true), '')::uuid[], '{}'::uuid[]))::uuid[]));
CREATE POLICY proxima_owner_write ON host_fixture.execution
    FOR ALL TO PUBLIC
    USING (owner_id = ANY((SELECT COALESCE(NULLIF(current_setting('app.write_owner', true), '')::uuid[], '{}'::uuid[]))::uuid[]))
    WITH CHECK (owner_id = ANY((SELECT COALESCE(NULLIF(current_setting('app.write_owner', true), '')::uuid[], '{}'::uuid[]))::uuid[]));
CREATE POLICY proxima_platform ON host_fixture.execution
    FOR ALL TO CURRENT_USER
    USING (current_setting('app.proxima_scope', true) = 'platform')
    WITH CHECK (current_setting('app.proxima_scope', true) = 'platform');

ALTER TABLE host_fixture.auxiliary ENABLE ROW LEVEL SECURITY;
ALTER TABLE host_fixture.auxiliary FORCE ROW LEVEL SECURITY;
CREATE POLICY proxima_owner_read ON host_fixture.auxiliary
    FOR SELECT TO PUBLIC
    USING (owner_id = ANY((SELECT COALESCE(NULLIF(current_setting('app.owner', true), '')::uuid[], '{}'::uuid[]))::uuid[]));
CREATE POLICY proxima_owner_write ON host_fixture.auxiliary
    FOR ALL TO PUBLIC
    USING (owner_id = ANY((SELECT COALESCE(NULLIF(current_setting('app.write_owner', true), '')::uuid[], '{}'::uuid[]))::uuid[]))
    WITH CHECK (owner_id = ANY((SELECT COALESCE(NULLIF(current_setting('app.write_owner', true), '')::uuid[], '{}'::uuid[]))::uuid[]));
CREATE POLICY proxima_platform ON host_fixture.auxiliary
    FOR ALL TO CURRENT_USER
    USING (current_setting('app.proxima_scope', true) = 'platform')
    WITH CHECK (current_setting('app.proxima_scope', true) = 'platform');

ALTER TABLE host_fixture.deferred_reference ENABLE ROW LEVEL SECURITY;
ALTER TABLE host_fixture.deferred_reference FORCE ROW LEVEL SECURITY;
CREATE POLICY proxima_owner_read ON host_fixture.deferred_reference
    FOR SELECT TO PUBLIC
    USING (EXISTS (
        SELECT 1 FROM host_fixture.execution AS e
        WHERE e.invocation_id = host_fixture.deferred_reference.invocation_id
          AND e.owner_id = ANY((SELECT COALESCE(NULLIF(current_setting('app.owner', true), '')::uuid[], '{}'::uuid[]))::uuid[])));
CREATE POLICY proxima_owner_write ON host_fixture.deferred_reference
    FOR ALL TO PUBLIC
    USING (EXISTS (
        SELECT 1 FROM host_fixture.execution AS e
        WHERE e.invocation_id = host_fixture.deferred_reference.invocation_id
          AND e.owner_id = ANY((SELECT COALESCE(NULLIF(current_setting('app.write_owner', true), '')::uuid[], '{}'::uuid[]))::uuid[])))
    WITH CHECK (EXISTS (
        SELECT 1 FROM host_fixture.execution AS e
        WHERE e.invocation_id = host_fixture.deferred_reference.invocation_id
          AND e.owner_id = ANY((SELECT COALESCE(NULLIF(current_setting('app.write_owner', true), '')::uuid[], '{}'::uuid[]))::uuid[])));
CREATE POLICY proxima_platform ON host_fixture.deferred_reference
    FOR ALL TO CURRENT_USER
    USING (current_setting('app.proxima_scope', true) = 'platform')
    WITH CHECK (current_setting('app.proxima_scope', true) = 'platform');
