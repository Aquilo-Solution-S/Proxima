-- proxima-example baseline: a test and tutorial fixture, not a product.
CREATE SCHEMA proxima_example;
CREATE TABLE proxima_example.notebooks (
    notebook_id uuid PRIMARY KEY,
    owner_id uuid NOT NULL,
    name text NOT NULL
);
CREATE TABLE proxima_example.settings (
    key text PRIMARY KEY,
    value text NOT NULL
);
CREATE TABLE proxima_example.note_v1 (
    t uuid PRIMARY KEY REFERENCES proxima_core.memory (t),
    notebook_id uuid NOT NULL,
    key text NOT NULL,
    body text NOT NULL
);
CREATE TABLE proxima_example.digest_v1 (
    t uuid PRIMARY KEY REFERENCES proxima_core.memory (t),
    notebook_id uuid NOT NULL,
    summary text NOT NULL
);
INSERT INTO proxima_core.flavor_surface (table_name, flavor_id) VALUES
    ('proxima_example.note_v1', 'proxima-example'),
    ('proxima_example.digest_v1', 'proxima-example');
CREATE OR REPLACE TRIGGER digest_v1_declared_by_memory
    BEFORE INSERT ON proxima_example.digest_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');
CREATE OR REPLACE TRIGGER note_v1_declared_by_memory
    BEFORE INSERT ON proxima_example.note_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');
-- owner-rls: begin
SELECT proxima_core.install_owner_rls('proxima_example', ARRAY['notebooks'], ARRAY['note_v1', 'digest_v1'], ARRAY['settings']);
SELECT proxima_core.assert_owner_rls_census('proxima_example');
-- owner-rls: end
DROP TRIGGER IF EXISTS memory_declares_proxima_example_digest_v1 ON proxima_core.memory;
CREATE CONSTRAINT TRIGGER memory_declares_proxima_example_digest_v1
    AFTER INSERT OR UPDATE OF sidecar_tables ON proxima_core.memory
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    WHEN ('proxima_example.digest_v1' = ANY (NEW.sidecar_tables))
    EXECUTE FUNCTION proxima_core.assert_declared_sidecar_present('proxima_example.digest_v1', 't');
DROP TRIGGER IF EXISTS digest_v1_declared_by_memory_on_delete ON proxima_example.digest_v1;
CREATE CONSTRAINT TRIGGER digest_v1_declared_by_memory_on_delete
    AFTER DELETE ON proxima_example.digest_v1
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_row_not_still_declared('proxima_example.digest_v1', 't');
DROP TRIGGER IF EXISTS memory_declares_proxima_example_note_v1 ON proxima_core.memory;
CREATE CONSTRAINT TRIGGER memory_declares_proxima_example_note_v1
    AFTER INSERT OR UPDATE OF sidecar_tables ON proxima_core.memory
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    WHEN ('proxima_example.note_v1' = ANY (NEW.sidecar_tables))
    EXECUTE FUNCTION proxima_core.assert_declared_sidecar_present('proxima_example.note_v1', 't');
DROP TRIGGER IF EXISTS note_v1_declared_by_memory_on_delete ON proxima_example.note_v1;
CREATE CONSTRAINT TRIGGER note_v1_declared_by_memory_on_delete
    AFTER DELETE ON proxima_example.note_v1
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_row_not_still_declared('proxima_example.note_v1', 't');
CREATE OR REPLACE TRIGGER digest_v1_declared_by_memory_on_update
    BEFORE UPDATE OF t ON proxima_example.digest_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');
CREATE OR REPLACE TRIGGER note_v1_declared_by_memory_on_update
    BEFORE UPDATE OF t ON proxima_example.note_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');
