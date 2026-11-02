-- Generated current-name tutorial fixture; applied migrations remain immutable.
-- proxiima-example baseline: a test and tutorial fixture, not a product.
CREATE SCHEMA proxiima_example;
CREATE TABLE proxiima_example.notebooks (
    notebook_id uuid PRIMARY KEY,
    owner_id uuid NOT NULL,
    name text NOT NULL
);
CREATE TABLE proxiima_example.settings (
    key text PRIMARY KEY,
    value text NOT NULL
);
CREATE TABLE proxiima_example.note_v1 (
    t uuid PRIMARY KEY REFERENCES proxiima_core.memory (t),
    notebook_id uuid NOT NULL,
    key text NOT NULL,
    body text NOT NULL,
    written_at timestamptz NOT NULL
);
CREATE TABLE proxiima_example.digest_v1 (
    t uuid PRIMARY KEY REFERENCES proxiima_core.memory (t),
    notebook_id uuid NOT NULL,
    summary text NOT NULL
);
INSERT INTO proxiima_core.spiice_surface (table_name, spiice_id) VALUES
    ('proxiima_example.note_v1', 'proxiima-example'),
    ('proxiima_example.digest_v1', 'proxiima-example');
CREATE OR REPLACE TRIGGER digest_v1_declared_by_memory
    BEFORE INSERT ON proxiima_example.digest_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxiima_core.assert_memory_declares_sidecar('t');
CREATE OR REPLACE TRIGGER note_v1_declared_by_memory
    BEFORE INSERT ON proxiima_example.note_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxiima_core.assert_memory_declares_sidecar('t');
-- owner-rls: begin
SELECT proxiima_core.install_owner_rls('proxiima_example', ARRAY['notebooks'], ARRAY['note_v1', 'digest_v1'], ARRAY['settings']);
SELECT proxiima_core.assert_owner_rls_census('proxiima_example');
-- owner-rls: end
DROP TRIGGER IF EXISTS memory_declares_proxiima_example_digest_v1 ON proxiima_core.memory;
CREATE CONSTRAINT TRIGGER memory_declares_proxiima_example_digest_v1
    AFTER INSERT OR UPDATE OF sidecar_tables ON proxiima_core.memory
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    WHEN ('proxiima_example.digest_v1' = ANY (NEW.sidecar_tables))
    EXECUTE FUNCTION proxiima_core.assert_declared_sidecar_present('proxiima_example.digest_v1', 't');
DROP TRIGGER IF EXISTS digest_v1_declared_by_memory_on_delete ON proxiima_example.digest_v1;
CREATE CONSTRAINT TRIGGER digest_v1_declared_by_memory_on_delete
    AFTER DELETE ON proxiima_example.digest_v1
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    EXECUTE FUNCTION proxiima_core.assert_row_not_still_declared('proxiima_example.digest_v1', 't');
DROP TRIGGER IF EXISTS memory_declares_proxiima_example_note_v1 ON proxiima_core.memory;
CREATE CONSTRAINT TRIGGER memory_declares_proxiima_example_note_v1
    AFTER INSERT OR UPDATE OF sidecar_tables ON proxiima_core.memory
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    WHEN ('proxiima_example.note_v1' = ANY (NEW.sidecar_tables))
    EXECUTE FUNCTION proxiima_core.assert_declared_sidecar_present('proxiima_example.note_v1', 't');
DROP TRIGGER IF EXISTS note_v1_declared_by_memory_on_delete ON proxiima_example.note_v1;
CREATE CONSTRAINT TRIGGER note_v1_declared_by_memory_on_delete
    AFTER DELETE ON proxiima_example.note_v1
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    EXECUTE FUNCTION proxiima_core.assert_row_not_still_declared('proxiima_example.note_v1', 't');
CREATE OR REPLACE TRIGGER digest_v1_declared_by_memory_on_update
    BEFORE UPDATE OF t ON proxiima_example.digest_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxiima_core.assert_memory_declares_sidecar('t');
CREATE OR REPLACE TRIGGER note_v1_declared_by_memory_on_update
    BEFORE UPDATE OF t ON proxiima_example.note_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxiima_core.assert_memory_declares_sidecar('t');
