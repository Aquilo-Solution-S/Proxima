-- Core trigger definitions. The definitions pass replaces only the core triggers declared here.
-- Constraint triggers are dropped immediately before recreation; no drop-all pass is required.
-- Every trigger is grouped under its owning proxima_core table.

-- agent_derivation_v1
-- Generated declaration family: integrity.rs::declaration_trigger.
CREATE OR REPLACE TRIGGER agent_derivation_v1_declared_by_memory
    BEFORE INSERT ON proxima_core.agent_derivation_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');
-- Generated delete guard family: integrity.rs::delete_guard_trigger.
DROP TRIGGER IF EXISTS agent_derivation_v1_declared_by_memory_on_delete ON proxima_core.agent_derivation_v1;
CREATE CONSTRAINT TRIGGER agent_derivation_v1_declared_by_memory_on_delete
    AFTER DELETE ON proxima_core.agent_derivation_v1
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_row_not_still_declared('proxima_core.agent_derivation_v1', 't');
-- Generated key repoint family: integrity.rs::key_repoint_trigger.
CREATE OR REPLACE TRIGGER agent_derivation_v1_declared_by_memory_on_update
    BEFORE UPDATE OF t ON proxima_core.agent_derivation_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');
CREATE OR REPLACE TRIGGER agent_derivation_v1_append_only BEFORE UPDATE ON proxima_core.agent_derivation_v1 FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();


-- agent_note_v1
-- Generated declaration family: integrity.rs::declaration_trigger.
CREATE OR REPLACE TRIGGER agent_note_v1_declared_by_memory
    BEFORE INSERT ON proxima_core.agent_note_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');
-- Generated delete guard family: integrity.rs::delete_guard_trigger.
DROP TRIGGER IF EXISTS agent_note_v1_declared_by_memory_on_delete ON proxima_core.agent_note_v1;
CREATE CONSTRAINT TRIGGER agent_note_v1_declared_by_memory_on_delete
    AFTER DELETE ON proxima_core.agent_note_v1
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_row_not_still_declared('proxima_core.agent_note_v1', 't');
-- Generated key repoint family: integrity.rs::key_repoint_trigger.
CREATE OR REPLACE TRIGGER agent_note_v1_declared_by_memory_on_update
    BEFORE UPDATE OF t ON proxima_core.agent_note_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');
CREATE OR REPLACE TRIGGER agent_note_v1_append_only BEFORE UPDATE ON proxima_core.agent_note_v1 FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();


-- delegated_authority_grants
CREATE OR REPLACE TRIGGER delegated_authority_writer_immutable
    BEFORE UPDATE OF "by", via ON proxima_core.delegated_authority_grants
    FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();


-- embedding_heads
CREATE OR REPLACE TRIGGER embedding_heads_deleted_index_debt
    AFTER DELETE ON proxima_core.embedding_heads
    REFERENCING OLD TABLE AS deleted_heads
    FOR EACH STATEMENT EXECUTE FUNCTION proxima_core.embedding_heads_deleted_index_debt();
CREATE OR REPLACE TRIGGER embedding_heads_moved_index_debt
    AFTER UPDATE OF owner_id ON proxima_core.embedding_heads
    FOR EACH ROW WHEN (OLD.owner_id IS DISTINCT FROM NEW.owner_id)
    EXECUTE FUNCTION proxima_core.embedding_head_moved_index_debt();


-- erased_pin_target
CREATE OR REPLACE TRIGGER erased_pin_target_append_only BEFORE DELETE OR UPDATE ON proxima_core.erased_pin_target FOR EACH ROW EXECUTE FUNCTION proxima_core.erased_pin_target_append_only();
CREATE OR REPLACE TRIGGER erased_pin_target_insert_guard BEFORE INSERT ON proxima_core.erased_pin_target FOR EACH ROW EXECUTE FUNCTION proxima_core.assert_erased_pin_target_insert();


-- goal
CREATE OR REPLACE TRIGGER goal_append_only BEFORE UPDATE ON proxima_core.goal FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();
CREATE OR REPLACE TRIGGER goal_erased_pin_target BEFORE DELETE ON proxima_core.goal FOR EACH ROW EXECUTE FUNCTION proxima_core.goal_erase_witness();
CREATE OR REPLACE TRIGGER goal_no_later_after_terminal BEFORE INSERT ON proxima_core.goal FOR EACH ROW EXECUTE FUNCTION proxima_core.goal_no_later_after_terminal();
CREATE OR REPLACE TRIGGER goal_parent_fixed BEFORE INSERT ON proxima_core.goal FOR EACH ROW EXECUTE FUNCTION proxima_core.goal_parent_fixed();
CREATE OR REPLACE TRIGGER goal_pin_target_checks BEFORE INSERT ON proxima_core.goal FOR EACH ROW EXECUTE FUNCTION proxima_core.goal_pin_target_checks();


-- goal_change_v1
-- Generated declaration family: integrity.rs::declaration_trigger.
CREATE OR REPLACE TRIGGER goal_change_v1_declared_by_memory
    BEFORE INSERT ON proxima_core.goal_change_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');
-- Generated delete guard family: integrity.rs::delete_guard_trigger.
DROP TRIGGER IF EXISTS goal_change_v1_declared_by_memory_on_delete ON proxima_core.goal_change_v1;
CREATE CONSTRAINT TRIGGER goal_change_v1_declared_by_memory_on_delete
    AFTER DELETE ON proxima_core.goal_change_v1
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_row_not_still_declared('proxima_core.goal_change_v1', 't');
-- Generated key repoint family: integrity.rs::key_repoint_trigger.
CREATE OR REPLACE TRIGGER goal_change_v1_declared_by_memory_on_update
    BEFORE UPDATE OF t ON proxima_core.goal_change_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');
CREATE OR REPLACE TRIGGER goal_change_v1_append_only BEFORE UPDATE ON proxima_core.goal_change_v1 FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();


-- goal_head
CREATE OR REPLACE TRIGGER goal_head_t_only BEFORE UPDATE ON proxima_core.goal_head FOR EACH ROW EXECUTE FUNCTION proxima_core.goal_head_t_only();


-- goal_replay_declaration
CREATE OR REPLACE TRIGGER goal_replay_declaration_append_only BEFORE UPDATE ON proxima_core.goal_replay_declaration FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();


-- group_edges
CREATE OR REPLACE TRIGGER group_edge_before_write BEFORE INSERT OR DELETE OR UPDATE ON proxima_core.group_edges FOR EACH ROW EXECUTE FUNCTION proxima_core.group_edge_before_write();
CREATE OR REPLACE TRIGGER group_edge_projection AFTER INSERT OR DELETE OR UPDATE ON proxima_core.group_edges FOR EACH ROW EXECUTE FUNCTION proxima_core.group_projection_after_write();
CREATE OR REPLACE TRIGGER group_edge_truncate_guard BEFORE TRUNCATE ON proxima_core.group_edges FOR EACH STATEMENT EXECUTE FUNCTION proxima_core.group_truncate_guard();


-- group_lock_request
CREATE OR REPLACE TRIGGER group_lock_request_cleanup AFTER INSERT ON proxima_core.group_lock_request FOR EACH ROW EXECUTE FUNCTION proxima_core.group_lock_request_cleanup();
CREATE OR REPLACE TRIGGER group_lock_request_trigger BEFORE INSERT ON proxima_core.group_lock_request FOR EACH ROW EXECUTE FUNCTION proxima_core.group_lock_request_trigger();


-- group_memberships
CREATE OR REPLACE TRIGGER group_membership_before_write BEFORE INSERT OR DELETE OR UPDATE ON proxima_core.group_memberships FOR EACH ROW EXECUTE FUNCTION proxima_core.group_membership_before_write();
CREATE OR REPLACE TRIGGER group_membership_projection AFTER INSERT OR DELETE OR UPDATE ON proxima_core.group_memberships FOR EACH ROW EXECUTE FUNCTION proxima_core.group_projection_after_write();
CREATE OR REPLACE TRIGGER group_membership_truncate_guard BEFORE TRUNCATE ON proxima_core.group_memberships FOR EACH STATEMENT EXECUTE FUNCTION proxima_core.group_truncate_guard();


-- group_role_projection
CREATE OR REPLACE TRIGGER group_projection_truncate_guard BEFORE TRUNCATE ON proxima_core.group_role_projection FOR EACH STATEMENT EXECUTE FUNCTION proxima_core.group_truncate_guard();
CREATE OR REPLACE TRIGGER group_projection_write_guard BEFORE INSERT OR DELETE OR UPDATE ON proxima_core.group_role_projection FOR EACH ROW EXECUTE FUNCTION proxima_core.group_projection_write_guard();


-- ingest_keys
CREATE OR REPLACE TRIGGER ingest_keys_append_only BEFORE UPDATE ON proxima_core.ingest_keys FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();


-- installation
CREATE OR REPLACE TRIGGER installation_identity_immutable BEFORE DELETE OR UPDATE ON proxima_core.installation FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_installation_immutable();


-- interpretation_v1
-- Generated declaration family: integrity.rs::declaration_trigger.
CREATE OR REPLACE TRIGGER interpretation_v1_declared_by_memory
    BEFORE INSERT ON proxima_core.interpretation_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');
-- Generated delete guard family: integrity.rs::delete_guard_trigger.
DROP TRIGGER IF EXISTS interpretation_v1_declared_by_memory_on_delete ON proxima_core.interpretation_v1;
CREATE CONSTRAINT TRIGGER interpretation_v1_declared_by_memory_on_delete
    AFTER DELETE ON proxima_core.interpretation_v1
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_row_not_still_declared('proxima_core.interpretation_v1', 't');
-- Generated key repoint family: integrity.rs::key_repoint_trigger.
CREATE OR REPLACE TRIGGER interpretation_v1_declared_by_memory_on_update
    BEFORE UPDATE OF t ON proxima_core.interpretation_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');
CREATE OR REPLACE TRIGGER interpretation_v1_append_only BEFORE UPDATE ON proxima_core.interpretation_v1 FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();


-- memory
-- Generated presence family: integrity.rs::presence_trigger.
DROP TRIGGER IF EXISTS memory_declares_proxima_core_agent_derivation_v1 ON proxima_core.memory;
CREATE CONSTRAINT TRIGGER memory_declares_proxima_core_agent_derivation_v1
    AFTER INSERT OR UPDATE OF sidecar_tables ON proxima_core.memory
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    WHEN ('proxima_core.agent_derivation_v1' = ANY (NEW.sidecar_tables))
    EXECUTE FUNCTION proxima_core.assert_declared_sidecar_present('proxima_core.agent_derivation_v1', 't');
DROP TRIGGER IF EXISTS memory_declares_proxima_core_agent_note_v1 ON proxima_core.memory;
CREATE CONSTRAINT TRIGGER memory_declares_proxima_core_agent_note_v1
    AFTER INSERT OR UPDATE OF sidecar_tables ON proxima_core.memory
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    WHEN ('proxima_core.agent_note_v1' = ANY (NEW.sidecar_tables))
    EXECUTE FUNCTION proxima_core.assert_declared_sidecar_present('proxima_core.agent_note_v1', 't');
DROP TRIGGER IF EXISTS memory_declares_proxima_core_interpretation_v1 ON proxima_core.memory;
CREATE CONSTRAINT TRIGGER memory_declares_proxima_core_interpretation_v1
    AFTER INSERT OR UPDATE OF sidecar_tables ON proxima_core.memory
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    WHEN ('proxima_core.interpretation_v1' = ANY (NEW.sidecar_tables))
    EXECUTE FUNCTION proxima_core.assert_declared_sidecar_present('proxima_core.interpretation_v1', 't');
DROP TRIGGER IF EXISTS memory_declares_proxima_core_utterance_v1 ON proxima_core.memory;
CREATE CONSTRAINT TRIGGER memory_declares_proxima_core_utterance_v1
    AFTER INSERT OR UPDATE OF sidecar_tables ON proxima_core.memory
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    WHEN ('proxima_core.utterance_v1' = ANY (NEW.sidecar_tables))
    EXECUTE FUNCTION proxima_core.assert_declared_sidecar_present('proxima_core.utterance_v1', 't');
DROP TRIGGER IF EXISTS memory_declares_proxima_core_write_act_v1 ON proxima_core.memory;
CREATE CONSTRAINT TRIGGER memory_declares_proxima_core_write_act_v1
    AFTER INSERT OR UPDATE OF sidecar_tables ON proxima_core.memory
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    WHEN ('proxima_core.write_act_v1' = ANY (NEW.sidecar_tables))
    EXECUTE FUNCTION proxima_core.assert_declared_sidecar_present('proxima_core.write_act_v1', 't');
DROP TRIGGER IF EXISTS memory_declares_proxima_core_goal_change_v1 ON proxima_core.memory;
CREATE CONSTRAINT TRIGGER memory_declares_proxima_core_goal_change_v1
    AFTER INSERT OR UPDATE OF sidecar_tables ON proxima_core.memory
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    WHEN ('proxima_core.goal_change_v1' = ANY (NEW.sidecar_tables))
    EXECUTE FUNCTION proxima_core.assert_declared_sidecar_present('proxima_core.goal_change_v1', 't');
CREATE OR REPLACE TRIGGER memory_align_head BEFORE INSERT ON proxima_core.memory FOR EACH ROW EXECUTE FUNCTION proxima_core.memory_align_head();
CREATE OR REPLACE TRIGGER memory_append_only BEFORE UPDATE ON proxima_core.memory FOR EACH ROW EXECUTE FUNCTION proxima_core.memory_owner_or_append_only();
CREATE OR REPLACE TRIGGER memory_erased_pin_target BEFORE DELETE ON proxima_core.memory FOR EACH ROW EXECUTE FUNCTION proxima_core.memory_erase_witness();
CREATE OR REPLACE TRIGGER memory_forget_grounding BEFORE UPDATE OF forgotten_at ON proxima_core.memory FOR EACH ROW WHEN (((old.forgotten_at IS NULL) AND (new.forgotten_at IS NOT NULL))) EXECUTE FUNCTION proxima_core.cooled_forget_grounding();
CREATE OR REPLACE TRIGGER memory_pin_checks BEFORE INSERT ON proxima_core.memory FOR EACH ROW EXECUTE FUNCTION proxima_core.memory_pin_checks();
CREATE OR REPLACE TRIGGER memory_sidecar_stamp_declared BEFORE INSERT OR UPDATE OF sidecar_tables ON proxima_core.memory FOR EACH ROW WHEN ((new.sidecar_tables <> '{}'::text[])) EXECUTE FUNCTION proxima_core.assert_sidecar_stamp_declared();


-- memory_head
CREATE OR REPLACE TRIGGER memory_head_t_only BEFORE UPDATE ON proxima_core.memory_head FOR EACH ROW EXECUTE FUNCTION proxima_core.memory_head_t_only();


-- owners
CREATE OR REPLACE TRIGGER owner_group_kind_guard BEFORE INSERT OR UPDATE ON proxima_core.owners FOR EACH ROW EXECUTE FUNCTION proxima_core.owner_group_kind_guard();
CREATE OR REPLACE TRIGGER owners_append_only BEFORE UPDATE ON proxima_core.owners FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();


-- projection
-- Generated projection trigger: projection.rs::projection_artifacts.
CREATE OR REPLACE TRIGGER projection_remember_lang
    BEFORE INSERT ON proxima_core.projection
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.remember_lexical_language();

-- publication_origin
CREATE OR REPLACE TRIGGER publication_origin_identity_immutable BEFORE UPDATE ON proxima_core.publication_origin FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_publication_origin_immutable();


-- publication_outbox
CREATE OR REPLACE TRIGGER publication_outbox_capture_immutable BEFORE UPDATE ON proxima_core.publication_outbox FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_publication_immutable();


-- utterance_v1
-- Generated declaration family: integrity.rs::declaration_trigger.
CREATE OR REPLACE TRIGGER utterance_v1_declared_by_memory
    BEFORE INSERT ON proxima_core.utterance_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');
-- Generated delete guard family: integrity.rs::delete_guard_trigger.
DROP TRIGGER IF EXISTS utterance_v1_declared_by_memory_on_delete ON proxima_core.utterance_v1;
CREATE CONSTRAINT TRIGGER utterance_v1_declared_by_memory_on_delete
    AFTER DELETE ON proxima_core.utterance_v1
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_row_not_still_declared('proxima_core.utterance_v1', 't');
-- Generated key repoint family: integrity.rs::key_repoint_trigger.
CREATE OR REPLACE TRIGGER utterance_v1_declared_by_memory_on_update
    BEFORE UPDATE OF t ON proxima_core.utterance_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');
CREATE OR REPLACE TRIGGER utterance_v1_append_only BEFORE UPDATE ON proxima_core.utterance_v1 FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();

-- write_act_v1
-- Generated declaration family: integrity.rs::declaration_trigger.
CREATE OR REPLACE TRIGGER write_act_v1_declared_by_memory
    BEFORE INSERT ON proxima_core.write_act_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');
-- Generated delete guard family: integrity.rs::delete_guard_trigger.
DROP TRIGGER IF EXISTS write_act_v1_declared_by_memory_on_delete ON proxima_core.write_act_v1;
CREATE CONSTRAINT TRIGGER write_act_v1_declared_by_memory_on_delete
    AFTER DELETE ON proxima_core.write_act_v1
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_row_not_still_declared('proxima_core.write_act_v1', 't');
-- Generated key repoint family: integrity.rs::key_repoint_trigger.
CREATE OR REPLACE TRIGGER write_act_v1_declared_by_memory_on_update
    BEFORE UPDATE OF t ON proxima_core.write_act_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');

-- store_identity
CREATE OR REPLACE TRIGGER store_identity_immutable
    BEFORE UPDATE OR DELETE ON proxima_core.store_identity
    FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_store_identity_immutable();

-- publication_counter
CREATE OR REPLACE TRIGGER publication_counter_forward_only
    BEFORE UPDATE ON proxima_core.publication_counter
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.publication_counter_forward_only();
