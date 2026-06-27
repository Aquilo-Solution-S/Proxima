-- 0006_entry_grant_relation_check.sql
--
-- Defense in depth: engine verbs already restrict memory-level grants to the
-- read/write chain; storage rejects any direct insert that bypasses them.

ALTER TABLE proxima_core.access_grants
    ADD CONSTRAINT access_grants_memory_relation_chk
    CHECK (
        resource_kind <> 'memory'
        OR relation = 'editor'
        OR relation = 'viewer'
    );
