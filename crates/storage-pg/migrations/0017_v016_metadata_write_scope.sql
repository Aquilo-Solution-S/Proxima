-- v0.0.16, third core file: owner scope writes no deployment metadata but
-- the one row ingest needs.
--
-- 0014 gave the three metadata tables an owner-write policy as wide as their
-- read policy, so any owner-scoped transaction could rewrite the deployment's
-- lexical default or the flavor surface registry. The only owner-scoped
-- writer is `remember_lexical_language`, which inserts a language on first
-- use. Everything else here is platform maintenance: `set_lexical_config`,
-- `lexical_language_forget`, and the migrations that register surfaces.
--
-- A FOR ALL policy's USING selects the rows UPDATE and DELETE touch and its
-- WITH CHECK admits new rows, so `USING (false)` keeps the insert and
-- refuses the rest. The platform policy is unchanged.

ALTER POLICY proxima_owner_write ON proxima_core.flavor_surface
    USING (false) WITH CHECK (false);
ALTER POLICY proxima_owner_write ON proxima_core.lexical_default
    USING (false) WITH CHECK (false);
ALTER POLICY proxima_owner_write ON proxima_core.lexical_languages
    USING (false);
