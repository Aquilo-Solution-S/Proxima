-- A Follows stamp row is visible exactly when its memory is (#474): stamp
-- moves to the memory_owner class, whose policies are EXISTS over
-- proxima_core.memory. memory's own owner policies apply inside that
-- subquery, so a forgotten memory hides its stamp from owner reads and the
-- lifecycle scope (restore, erase) still sees it. Follows keeps owner_id equal
-- to the memory's owner, so the owner check is the same; the probe is one
-- memory primary-key lookup per row. Re-run the installer (idempotent).
SELECT proxima_core.install_owner_rls(
    'proxima_example',
    ARRAY['notebooks'],
    ARRAY['note_v1', 'digest_v1'],
    ARRAY['settings'],
    ARRAY['stamp_v1']
);
SELECT proxima_core.assert_owner_rls_census('proxima_example');
