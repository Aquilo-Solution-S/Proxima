-- Stamp sidecar that follows its memory on transfer (#474).
CREATE TABLE proxima_example.stamp_v1 (
    t uuid PRIMARY KEY REFERENCES proxima_core.memory (t),
    owner_id uuid NOT NULL,
    mark text NOT NULL
);
INSERT INTO proxima_core.flavor_surface (table_name, flavor_id) VALUES
    ('proxima_example.stamp_v1', 'proxima-example');
CREATE OR REPLACE TRIGGER stamp_v1_declared_by_memory
    BEFORE INSERT ON proxima_example.stamp_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');
-- Re-run owner RLS with stamp in the owner_id class (installer is idempotent).
SELECT proxima_core.install_owner_rls(
    'proxima_example',
    ARRAY['notebooks', 'stamp_v1'],
    ARRAY['note_v1', 'digest_v1'],
    ARRAY['settings']
);
SELECT proxima_core.assert_owner_rls_census('proxima_example');
DROP TRIGGER IF EXISTS memory_declares_proxima_example_stamp_v1 ON proxima_core.memory;
CREATE CONSTRAINT TRIGGER memory_declares_proxima_example_stamp_v1
    AFTER INSERT OR UPDATE OF sidecar_tables ON proxima_core.memory
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    WHEN ('proxima_example.stamp_v1' = ANY (NEW.sidecar_tables))
    EXECUTE FUNCTION proxima_core.assert_declared_sidecar_present('proxima_example.stamp_v1', 't');
DROP TRIGGER IF EXISTS stamp_v1_declared_by_memory_on_delete ON proxima_example.stamp_v1;
CREATE CONSTRAINT TRIGGER stamp_v1_declared_by_memory_on_delete
    AFTER DELETE ON proxima_example.stamp_v1
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_row_not_still_declared('proxima_example.stamp_v1', 't');
CREATE OR REPLACE TRIGGER stamp_v1_declared_by_memory_on_update
    BEFORE UPDATE OF t ON proxima_example.stamp_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');
