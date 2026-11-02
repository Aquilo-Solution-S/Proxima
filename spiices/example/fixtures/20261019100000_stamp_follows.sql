-- Generated current-name tutorial fixture; applied migrations remain immutable.
-- Stamp sidecar that follows its memory on transfer (#474).
CREATE TABLE proxiima_example.stamp_v1 (
    t uuid PRIMARY KEY REFERENCES proxiima_core.memory (t),
    owner_id uuid NOT NULL,
    mark text NOT NULL
);
INSERT INTO proxiima_core.spiice_surface (table_name, spiice_id) VALUES
    ('proxiima_example.stamp_v1', 'proxiima-example');
CREATE OR REPLACE TRIGGER stamp_v1_declared_by_memory
    BEFORE INSERT ON proxiima_example.stamp_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxiima_core.assert_memory_declares_sidecar('t');
-- Re-run owner RLS with stamp in the owner_id class (installer is idempotent).
SELECT proxiima_core.install_owner_rls(
    'proxiima_example',
    ARRAY['notebooks', 'stamp_v1'],
    ARRAY['note_v1', 'digest_v1'],
    ARRAY['settings']
);
SELECT proxiima_core.assert_owner_rls_census('proxiima_example');
DROP TRIGGER IF EXISTS memory_declares_proxiima_example_stamp_v1 ON proxiima_core.memory;
CREATE CONSTRAINT TRIGGER memory_declares_proxiima_example_stamp_v1
    AFTER INSERT OR UPDATE OF sidecar_tables ON proxiima_core.memory
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    WHEN ('proxiima_example.stamp_v1' = ANY (NEW.sidecar_tables))
    EXECUTE FUNCTION proxiima_core.assert_declared_sidecar_present('proxiima_example.stamp_v1', 't');
DROP TRIGGER IF EXISTS stamp_v1_declared_by_memory_on_delete ON proxiima_example.stamp_v1;
CREATE CONSTRAINT TRIGGER stamp_v1_declared_by_memory_on_delete
    AFTER DELETE ON proxiima_example.stamp_v1
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    EXECUTE FUNCTION proxiima_core.assert_row_not_still_declared('proxiima_example.stamp_v1', 't');
CREATE OR REPLACE TRIGGER stamp_v1_declared_by_memory_on_update
    BEFORE UPDATE OF t ON proxiima_example.stamp_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxiima_core.assert_memory_declares_sidecar('t');
