-- v0.0.35: origins that live in other stores (issue #462).
--
-- A derived memory may be made from memories of another store reached
-- through the host's OtherStores port. Those origins are not proxima_core
-- memories, so they cannot ride the row's own `origins` pin column, whose
-- integrity trigger (0003) is untouched. Each one is a row here instead:
-- the local memory `t`, the store name and the store's own handle.
--
-- There is deliberately no foreign key: nothing on the remote side is ever
-- checked, copied or cached, and the local `t` is bound by the owner RLS
-- policy of its memory (class memory_owner, 0023's rule keyed on `t`).
-- Rows are written once, in the transaction that admits the memory.
CREATE TABLE IF NOT EXISTS proxima_core.memory_external_origin (
    t uuid NOT NULL,
    store text NOT NULL
        CONSTRAINT memory_external_origin_store_chk
        CHECK (store ~ '^[a-z][a-z0-9_-]{1,63}$' AND store <> 'local'),
    handle text NOT NULL
        CONSTRAINT memory_external_origin_handle_chk
        CHECK (char_length(handle) BETWEEN 1 AND 512),
    PRIMARY KEY (t, store, handle)
);

-- Owner RLS through 0024's per-table building blocks, as 0026 does. The
-- fixture lane that skips the owner-RLS migrations has no appliers.
DO $external_origin_rls$
BEGIN
    IF EXISTS (
        SELECT 1
          FROM pg_catalog.pg_proc AS p
          JOIN pg_catalog.pg_namespace AS n ON n.oid = p.pronamespace
         WHERE n.nspname = 'proxima_core' AND p.proname = 'owner_rls_apply_class'
    ) THEN
        PERFORM proxima_core.owner_rls_apply_class(
            '0029', 'class memory_owner', 'proxima_core', 'memory_external_origin', 'memory_owner', NULL);
        PERFORM proxima_core.owner_rls_census_table('proxima_core', 'memory_external_origin');
    END IF;
END
$external_origin_rls$;
