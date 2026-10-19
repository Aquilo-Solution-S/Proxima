-- #474: the host, not the flavor, decides which tables are Follows sidecars.
--
--   owner_follows   one row per `pg_sidecar!(owner: follows)` table the host
--                   composes, written by the host from its Rust registry
--                   through proxima_core.sync_owner_follows. Flavors never
--                   write it.
--
-- A marked table gets the host's follows owner RLS: its own owner_id check
-- AND an EXISTS over its proxima_core.memory row, so a forgotten memory hides
-- it from owner reads. install_owner_rls gives a marked table that rule
-- whatever class the flavor lists it in, so the class a flavor migration
-- names cannot leak forgotten rows.
--
-- Text names and no FK: the host marks a table before the flavor lane that
-- creates it runs. Additive and idempotent. The owner RLS lives in
-- definitions/ and is installed by the definitions pass after this
-- migration.
SELECT set_config('app.proxima_scope', 'platform', true);

CREATE TABLE IF NOT EXISTS proxima_core.owner_follows (
    table_name text PRIMARY KEY
        CONSTRAINT owner_follows_qualified_chk
        CHECK (table_name LIKE '%.%' AND table_name = lower(table_name))
);

COMMENT ON TABLE proxima_core.owner_follows IS 'Follows sidecar tables, one row per schema-qualified table. Written by the host from pg_sidecar!(owner: follows) through sync_owner_follows; flavors never write it. A listed table gets the host follows owner RLS from install_owner_rls whatever class it is listed in.';
