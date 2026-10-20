-- Core row-level security policies. The installer replaces only the three policies declared for each table.
-- Every table is installed once through owner_rls_apply_class; exceptional expressions follow each call.

-- agent_derivation_v1
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class fk_parent', 'proxima_core', 'agent_derivation_v1', 'fk_parent', NULL);

-- agent_note_v1
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class fk_parent', 'proxima_core', 'agent_note_v1', 'fk_parent', NULL);

-- blob
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class owner_id', 'proxima_core', 'blob', 'owner_id', 'owner_id');

-- blob_uploads
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class owner_id', 'proxima_core', 'blob_uploads', 'owner_id', 'owner_id');

-- closed_handle
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class ownerless', 'proxima_core', 'closed_handle', 'ownerless', NULL);

ALTER POLICY proxima_owner_read ON proxima_core.closed_handle USING ((EXISTS ( SELECT 1
   FROM proxima_core.memory_head parent
  WHERE ((parent.handle = closed_handle.handle) AND (parent.owner_id = ANY (
        CASE (parent.kind)::text
            WHEN 'fact'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
            WHEN 'abstraction'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.read_abstraction'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
            WHEN 'perspective'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.read_perspective'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
            ELSE ( SELECT COALESCE((NULLIF(current_setting('app.read_goal'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
        END))))));

ALTER POLICY proxima_owner_write ON proxima_core.closed_handle USING ((EXISTS ( SELECT 1
   FROM proxima_core.memory_head parent
  WHERE ((parent.handle = closed_handle.handle) AND (parent.owner_id = ANY (
        CASE (parent.kind)::text
            WHEN 'fact'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.write_owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
            WHEN 'abstraction'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.write_abstraction'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
            WHEN 'perspective'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.write_perspective'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
            ELSE ( SELECT COALESCE((NULLIF(current_setting('app.write_goal'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
        END)))))) WITH CHECK ((EXISTS ( SELECT 1
   FROM proxima_core.memory_head parent
  WHERE ((parent.handle = closed_handle.handle) AND (parent.owner_id = ANY (
        CASE (parent.kind)::text
            WHEN 'fact'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.write_owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
            WHEN 'abstraction'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.write_abstraction'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
            WHEN 'perspective'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.write_perspective'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
            ELSE ( SELECT COALESCE((NULLIF(current_setting('app.write_goal'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
        END))))));

-- cold_purge_pending
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class owner_id', 'proxima_core', 'cold_purge_pending', 'owner_id', 'owner_id');

-- content
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class owner_id', 'proxima_core', 'content', 'owner_id', 'owner_id');

ALTER POLICY proxima_owner_read ON proxima_core.content USING (((owner_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.read_perspective'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[])) OR ((owner_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[])) AND ((EXISTS ( SELECT 1
   FROM proxima_core.memory parent
  WHERE ((parent.content_id = content.content_id) AND (parent.owner_id = ANY (
        CASE (parent.kind)::text
            WHEN 'fact'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
            WHEN 'abstraction'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.read_abstraction'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
            WHEN 'perspective'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.read_perspective'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
            ELSE ( SELECT COALESCE((NULLIF(current_setting('app.read_goal'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
        END)))))))));

ALTER POLICY proxima_owner_write ON proxima_core.content USING (((owner_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.write_perspective'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[])) OR ((owner_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.write_owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[])) AND ((EXISTS ( SELECT 1
   FROM proxima_core.memory parent
  WHERE ((parent.content_id = content.content_id) AND (parent.owner_id = ANY (
        CASE (parent.kind)::text
            WHEN 'fact'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.write_owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
            WHEN 'abstraction'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.write_abstraction'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
            WHEN 'perspective'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.write_perspective'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
            ELSE ( SELECT COALESCE((NULLIF(current_setting('app.write_goal'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
        END))))))))) WITH CHECK ((owner_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.write_owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[])));

-- delegated_authority_grants
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class owner_id', 'proxima_core', 'delegated_authority_grants', 'owner_id', 'owner_id');

-- embedding_heads
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class owner_id', 'proxima_core', 'embedding_heads', 'owner_id', 'owner_id');

-- embedding_jobs
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class owner_id', 'proxima_core', 'embedding_jobs', 'owner_id', 'owner_id');

-- embeddings
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class owner_id', 'proxima_core', 'embeddings', 'owner_id', 'owner_id');

-- erased_pin_target
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class ownerless', 'proxima_core', 'erased_pin_target', 'ownerless', NULL);

-- flavor_surface
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class ownerless', 'proxima_core', 'flavor_surface', 'ownerless', NULL);

ALTER POLICY proxima_owner_read ON proxima_core.flavor_surface USING (((current_setting('app.proxima_scope'::text, true) = 'owner'::text) AND (cardinality(COALESCE((NULLIF(current_setting('app.owner'::text, true), ''::text))::uuid[], '{}'::uuid[])) > 0)));

-- goal
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class owner_id', 'proxima_core', 'goal', 'owner_id', 'owner_id');

ALTER POLICY proxima_owner_read ON proxima_core.goal USING ((owner_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.read_goal'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[])));

ALTER POLICY proxima_owner_write ON proxima_core.goal USING ((owner_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.write_goal'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[]))) WITH CHECK ((owner_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.write_goal'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[])));

-- goal_change_v1
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class fk_parent', 'proxima_core', 'goal_change_v1', 'fk_parent', NULL);

-- goal_head
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class owner_id', 'proxima_core', 'goal_head', 'owner_id', 'owner_id');

ALTER POLICY proxima_owner_read ON proxima_core.goal_head USING ((owner_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.read_goal'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[])));

ALTER POLICY proxima_owner_write ON proxima_core.goal_head USING ((owner_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.write_goal'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[]))) WITH CHECK ((owner_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.write_goal'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[])));

-- goal_replay_declaration
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class fk_parent', 'proxima_core', 'goal_replay_declaration', 'fk_parent', NULL);

-- group_edges
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class ownerless', 'proxima_core', 'group_edges', 'ownerless', NULL);

ALTER POLICY proxima_owner_read ON proxima_core.group_edges USING (((child_group_id = ANY (COALESCE((NULLIF(current_setting('app.owner'::text, true), ''::text))::uuid[], '{}'::uuid[]))) OR (parent_group_id = ANY (COALESCE((NULLIF(current_setting('app.owner'::text, true), ''::text))::uuid[], '{}'::uuid[])))));

ALTER POLICY proxima_owner_write ON proxima_core.group_edges USING (((parent_group_id = ANY (COALESCE((NULLIF(current_setting('app.manage_owner'::text, true), ''::text))::uuid[], '{}'::uuid[]))) OR (child_group_id = ANY (COALESCE((NULLIF(current_setting('app.manage_owner'::text, true), ''::text))::uuid[], '{}'::uuid[]))))) WITH CHECK (((parent_group_id = ANY (COALESCE((NULLIF(current_setting('app.manage_owner'::text, true), ''::text))::uuid[], '{}'::uuid[]))) AND (child_group_id = ANY (COALESCE((NULLIF(current_setting('app.manage_owner'::text, true), ''::text))::uuid[], '{}'::uuid[])))));

-- group_lock_request
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class ownerless', 'proxima_core', 'group_lock_request', 'ownerless', NULL);

ALTER POLICY proxima_owner_read ON proxima_core.group_lock_request USING (((left_group_id = ANY (COALESCE((NULLIF(current_setting('app.manage_owner'::text, true), ''::text))::uuid[], '{}'::uuid[]))) OR (right_group_id = ANY (COALESCE((NULLIF(current_setting('app.manage_owner'::text, true), ''::text))::uuid[], '{}'::uuid[])))));

ALTER POLICY proxima_owner_write ON proxima_core.group_lock_request USING (((left_group_id = ANY (COALESCE((NULLIF(current_setting('app.manage_owner'::text, true), ''::text))::uuid[], '{}'::uuid[]))) OR (right_group_id = ANY (COALESCE((NULLIF(current_setting('app.manage_owner'::text, true), ''::text))::uuid[], '{}'::uuid[]))))) WITH CHECK (((left_group_id = ANY (COALESCE((NULLIF(current_setting('app.manage_owner'::text, true), ''::text))::uuid[], '{}'::uuid[]))) OR (right_group_id = ANY (COALESCE((NULLIF(current_setting('app.manage_owner'::text, true), ''::text))::uuid[], '{}'::uuid[])))));

-- group_memberships
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class ownerless', 'proxima_core', 'group_memberships', 'ownerless', NULL);

ALTER POLICY proxima_owner_read ON proxima_core.group_memberships USING ((group_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[])));

ALTER POLICY proxima_owner_write ON proxima_core.group_memberships USING ((group_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.manage_owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[]))) WITH CHECK ((group_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.manage_owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[])));

-- group_role_projection
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class ownerless', 'proxima_core', 'group_role_projection', 'ownerless', NULL);

ALTER POLICY proxima_owner_read ON proxima_core.group_role_projection USING (
    group_id = ANY (COALESCE(NULLIF(current_setting('app.owner', true), '')::uuid[], '{}'::uuid[]))
    OR (member_user_id = NULLIF(current_setting('app.authority_subject', true), '')::uuid
        AND member_user_id = NULLIF(current_setting('app.subject', true), '')::uuid)
);

ALTER POLICY proxima_owner_write ON proxima_core.group_role_projection USING ((group_id = ANY (COALESCE((NULLIF(current_setting('app.manage_owner'::text, true), ''::text))::uuid[], '{}'::uuid[])))) WITH CHECK ((group_id = ANY (COALESCE((NULLIF(current_setting('app.manage_owner'::text, true), ''::text))::uuid[], '{}'::uuid[]))));

-- ingest_keys
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class owner_id', 'proxima_core', 'ingest_keys', 'owner_id', 'owner_id');

-- installation
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class ownerless', 'proxima_core', 'installation', 'ownerless', NULL);

-- interpretation_v1
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class fk_parent', 'proxima_core', 'interpretation_v1', 'fk_parent', NULL);

-- lexical_default
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class ownerless', 'proxima_core', 'lexical_default', 'ownerless', NULL);

ALTER POLICY proxima_owner_read ON proxima_core.lexical_default USING (((current_setting('app.proxima_scope'::text, true) = 'owner'::text) AND (cardinality(COALESCE((NULLIF(current_setting('app.owner'::text, true), ''::text))::uuid[], '{}'::uuid[])) > 0)));

-- lexical_languages
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class ownerless', 'proxima_core', 'lexical_languages', 'ownerless', NULL);

ALTER POLICY proxima_owner_read ON proxima_core.lexical_languages USING (((current_setting('app.proxima_scope'::text, true) = 'owner'::text) AND (cardinality(COALESCE((NULLIF(current_setting('app.owner'::text, true), ''::text))::uuid[], '{}'::uuid[])) > 0)));

ALTER POLICY proxima_owner_write ON proxima_core.lexical_languages USING (false) WITH CHECK (((current_setting('app.proxima_scope'::text, true) = 'owner'::text) AND (cardinality(COALESCE((NULLIF(current_setting('app.write_owner'::text, true), ''::text))::uuid[], '{}'::uuid[])) > 0)));

-- memory
-- A forgotten Memory (forgotten_at set) is hidden from owner reads and
-- writes, and from every sidecar policy that reaches it through memory, unless
-- the transaction runs in the lifecycle scope (PgLifecycleScope, #488).
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class owner_id', 'proxima_core', 'memory', 'owner_id', 'owner_id');

ALTER POLICY proxima_owner_read ON proxima_core.memory USING ((((owner_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[])) AND (((kind = 'fact'::proxima_core.memory_kind) AND (owner_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[]))) OR ((kind = 'abstraction'::proxima_core.memory_kind) AND (owner_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.read_abstraction'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[]))) OR ((kind = 'perspective'::proxima_core.memory_kind) AND (owner_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.read_perspective'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[]))))) AND ((forgotten_at IS NULL) OR (current_setting('app.proxima_lifecycle'::text, true) = 'on'::text))));

ALTER POLICY proxima_owner_write ON proxima_core.memory USING ((((owner_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.write_owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[])) AND (((kind = 'fact'::proxima_core.memory_kind) AND (owner_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.write_owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[]))) OR ((kind = 'abstraction'::proxima_core.memory_kind) AND (owner_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.write_abstraction'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[]))) OR ((kind = 'perspective'::proxima_core.memory_kind) AND (owner_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.write_perspective'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[]))))) AND ((forgotten_at IS NULL) OR (current_setting('app.proxima_lifecycle'::text, true) = 'on'::text)))) WITH CHECK ((((owner_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.write_owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[])) AND (((kind = 'fact'::proxima_core.memory_kind) AND (owner_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.write_owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[]))) OR ((kind = 'abstraction'::proxima_core.memory_kind) AND (owner_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.write_abstraction'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[]))) OR ((kind = 'perspective'::proxima_core.memory_kind) AND (owner_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.write_perspective'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[]))))) AND ((forgotten_at IS NULL) OR (current_setting('app.proxima_lifecycle'::text, true) = 'on'::text))));

-- memory_external_origin
-- Reached through the memory it describes (#478). Forget keeps the memory
-- row (#488), so a forgotten parent still has its owner; owner reads hide it
-- with the parent, lifecycle reads (export, erase, transfer) see it.
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class memory_owner', 'proxima_core', 'memory_external_origin', 'memory_owner', NULL);

ALTER POLICY proxima_owner_read ON proxima_core.memory_external_origin USING (((EXISTS ( SELECT 1
   FROM proxima_core.memory parent
  WHERE ((parent.t = memory_external_origin.t) AND (parent.owner_id = ANY (
        CASE (parent.kind)::text
            WHEN 'fact'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
            WHEN 'abstraction'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.read_abstraction'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
            ELSE ( SELECT COALESCE((NULLIF(current_setting('app.read_perspective'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
        END)))))));

ALTER POLICY proxima_owner_write ON proxima_core.memory_external_origin USING (((EXISTS ( SELECT 1
   FROM proxima_core.memory parent
  WHERE ((parent.t = memory_external_origin.t) AND (parent.owner_id = ANY (
        CASE (parent.kind)::text
            WHEN 'fact'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.write_owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
            WHEN 'abstraction'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.write_abstraction'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
            ELSE ( SELECT COALESCE((NULLIF(current_setting('app.write_perspective'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
        END))))))) WITH CHECK ((EXISTS ( SELECT 1
   FROM proxima_core.memory parent
  WHERE ((parent.t = memory_external_origin.t) AND (parent.owner_id = ANY (
        CASE (parent.kind)::text
            WHEN 'fact'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.write_owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
            WHEN 'abstraction'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.write_abstraction'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
            ELSE ( SELECT COALESCE((NULLIF(current_setting('app.write_perspective'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
        END))))));

-- memory_head
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class owner_id', 'proxima_core', 'memory_head', 'owner_id', 'owner_id');

ALTER POLICY proxima_owner_read ON proxima_core.memory_head USING (((owner_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[])) AND (owner_id = ANY (
CASE (kind)::text
    WHEN 'fact'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
    WHEN 'abstraction'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.read_abstraction'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
    WHEN 'perspective'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.read_perspective'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
    ELSE ( SELECT COALESCE((NULLIF(current_setting('app.read_goal'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
END))));

ALTER POLICY proxima_owner_write ON proxima_core.memory_head USING (((owner_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.write_owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[])) AND (owner_id = ANY (
CASE (kind)::text
    WHEN 'fact'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.write_owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
    WHEN 'abstraction'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.write_abstraction'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
    WHEN 'perspective'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.write_perspective'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
    ELSE ( SELECT COALESCE((NULLIF(current_setting('app.write_goal'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
END)))) WITH CHECK (((owner_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.write_owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[])) AND (owner_id = ANY (
CASE (kind)::text
    WHEN 'fact'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.write_owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
    WHEN 'abstraction'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.write_abstraction'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
    WHEN 'perspective'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.write_perspective'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
    ELSE ( SELECT COALESCE((NULLIF(current_setting('app.write_goal'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
END))));

-- owner_rls_epoch
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class ownerless', 'proxima_core', 'owner_rls_epoch', 'ownerless', NULL);

-- owners
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class owner_id', 'proxima_core', 'owners', 'owner_id', 'owner_id');

ALTER POLICY proxima_owner_read ON proxima_core.owners USING (
    owner_id = ANY (COALESCE(NULLIF(current_setting('app.owner', true), '')::uuid[], '{}'::uuid[]))
    OR (owner_id = NULLIF(current_setting('app.authority_subject', true), '')::uuid
        AND owner_id = NULLIF(current_setting('app.subject', true), '')::uuid)
);

-- projection
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class owner_id', 'proxima_core', 'projection', 'owner_id', 'owner_id');

-- publication_consumer
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class ownerless', 'proxima_core', 'publication_consumer', 'ownerless', NULL);

ALTER POLICY proxima_owner_read ON proxima_core.publication_consumer USING (((current_setting('app.proxima_scope'::text, true) = 'owner'::text) AND (cardinality(COALESCE((NULLIF(current_setting('app.owner'::text, true), ''::text))::uuid[], '{}'::uuid[])) > 0)));

-- publication_delivery
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class fk_parent', 'proxima_core', 'publication_delivery', 'fk_parent', NULL);

-- publication_origin
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class owner_id', 'proxima_core', 'publication_origin', 'owner_id', 'original_owner_id');

-- publication_outbox
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class owner_id', 'proxima_core', 'publication_outbox', 'owner_id', 'owner_id');

-- sketch
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class owner_id', 'proxima_core', 'sketch', 'owner_id', 'owner_id');

ALTER POLICY proxima_owner_read ON proxima_core.sketch USING (((owner_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[])) AND (owner_id = ANY (
CASE (kind)::text
    WHEN 'fact'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
    WHEN 'abstraction'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.read_abstraction'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
    WHEN 'perspective'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.read_perspective'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
    ELSE ( SELECT COALESCE((NULLIF(current_setting('app.read_goal'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
END))));

ALTER POLICY proxima_owner_write ON proxima_core.sketch USING (((owner_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.write_owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[])) AND (owner_id = ANY (
CASE (kind)::text
    WHEN 'fact'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.write_owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
    WHEN 'abstraction'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.write_abstraction'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
    WHEN 'perspective'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.write_perspective'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
    ELSE ( SELECT COALESCE((NULLIF(current_setting('app.write_goal'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
END)))) WITH CHECK (((owner_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.write_owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[])) AND (owner_id = ANY (
CASE (kind)::text
    WHEN 'fact'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.write_owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
    WHEN 'abstraction'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.write_abstraction'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
    WHEN 'perspective'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.write_perspective'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
    ELSE ( SELECT COALESCE((NULLIF(current_setting('app.write_goal'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
END))));

-- source_cursors
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class owner_id', 'proxima_core', 'source_cursors', 'owner_id', 'owner_id');

-- task_goal_v1
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class fk_parent', 'proxima_core', 'task_goal_v1', 'fk_parent', NULL);

-- utterance_v1
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class fk_parent', 'proxima_core', 'utterance_v1', 'fk_parent', NULL);

-- write_act_v1
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class fk_parent', 'proxima_core', 'write_act_v1', 'fk_parent', NULL);

-- store_identity
SELECT proxima_core.owner_rls_apply_class(
    'policies.sql', 'class ownerless', 'proxima_core', 'store_identity', 'ownerless', NULL);

-- publication_counter
SELECT proxima_core.owner_rls_apply_class(
    'policies.sql', 'class owner_id', 'proxima_core', 'publication_counter', 'owner_id', 'owner_id');

-- owner_follows: the host writes it (platform); owner scopes may read it,
-- like flavor_surface.
SELECT proxima_core.owner_rls_apply_class(
    'policies.sql', 'class ownerless', 'proxima_core', 'owner_follows', 'ownerless', NULL);

ALTER POLICY proxima_owner_read ON proxima_core.owner_follows USING (((current_setting('app.proxima_scope'::text, true) = 'owner'::text) AND (cardinality(COALESCE((NULLIF(current_setting('app.owner'::text, true), ''::text))::uuid[], '{}'::uuid[])) > 0)));

-- export_erase_fence and maintenance_lease: platform coordination state about
-- no owner. Only the platform scope reaches them.
SELECT proxima_core.owner_rls_apply_class(
    'policies.sql', 'class ownerless', 'proxima_core', 'export_erase_fence', 'ownerless', NULL);

SELECT proxima_core.owner_rls_apply_class(
    'policies.sql', 'class ownerless', 'proxima_core', 'maintenance_lease', 'ownerless', NULL);

-- vector_index_debt: read and written at its entity's kind, as its head is;
-- the platform scope drains it.
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class owner_id', 'proxima_core', 'vector_index_debt', 'owner_id', 'owner_id');
