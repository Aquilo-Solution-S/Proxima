-- Pin temporary name lookup last in the platform-owned integrity routines.
-- 0014 is released: harden its settings without changing bodies, owners or ACLs.
ALTER FUNCTION proxima_core.record_erased_pin_target(uuid, proxima_core.pin_target_kind)
    SET search_path = pg_catalog, proxima_core, pg_temp;
ALTER FUNCTION proxima_core.assert_erased_pin_target_insert()
    SET search_path = pg_catalog, proxima_core, pg_temp;
ALTER FUNCTION proxima_core.memory_erase_witness()
    SET search_path = pg_catalog, proxima_core, pg_temp;
ALTER FUNCTION proxima_core.cooled_erase_witness()
    SET search_path = pg_catalog, proxima_core, pg_temp;
ALTER FUNCTION proxima_core.goal_erase_witness()
    SET search_path = pg_catalog, proxima_core, pg_temp;
ALTER FUNCTION proxima_core.cooled_identity_seal()
    SET search_path = pg_catalog, proxima_core, pg_temp;
ALTER FUNCTION proxima_core.cooled_forget_grounding()
    SET search_path = pg_catalog, proxima_core, pg_temp;
ALTER FUNCTION proxima_core.goal_pin_target_checks()
    SET search_path = pg_catalog, proxima_core, pg_temp;
ALTER FUNCTION proxima_core.wake_pin_target_checks()
    SET search_path = pg_catalog, proxima_core, pg_temp;
ALTER FUNCTION proxima_core.memory_pin_checks()
    SET search_path = pg_catalog, proxima_core, pg_temp;
ALTER FUNCTION proxima_core.pins_have_grounding_support(uuid[], uuid, proxima_core.memory_kind)
    SET search_path = pg_catalog, proxima_core, pg_temp;
