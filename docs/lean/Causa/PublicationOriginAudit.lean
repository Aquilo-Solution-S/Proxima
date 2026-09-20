import Causa.PublicationOrigin

namespace Causa.PublicationOrigin

/-- info: 'Causa.PublicationOrigin.SourceId.ofToken_preserves_native_token' does not depend on any axioms -/
#guard_msgs in
#print axioms SourceId.ofToken_preserves_native_token

/-- info: 'Causa.PublicationOrigin.SourceId.ofToken_injective' does not depend on any axioms -/
#guard_msgs in
#print axioms SourceId.ofToken_injective

/-- info: 'Causa.PublicationOrigin.fresh_admission_captures_atomically' does not depend on any axioms -/
#guard_msgs in
#print axioms fresh_admission_captures_atomically

/-- info: 'Causa.PublicationOrigin.live_fact_cannot_be_freshly_recaptured' does not depend on any axioms -/
#guard_msgs in
#print axioms live_fact_cannot_be_freshly_recaptured

/-- info: 'Causa.PublicationOrigin.hard_deleted_fact_cannot_be_freshly_recaptured' does not depend on any axioms -/
#guard_msgs in
#print axioms hard_deleted_fact_cannot_be_freshly_recaptured

/-- info: 'Causa.PublicationOrigin.fresh_capture_preserves_key_uniqueness' does not depend on any axioms -/
#guard_msgs in
#print axioms fresh_capture_preserves_key_uniqueness

/-- info: 'Causa.PublicationOrigin.fresh_capture_stamps_outbox_original_owner' does not depend on any axioms -/
#guard_msgs in
#print axioms fresh_capture_stamps_outbox_original_owner

/-- info: 'Causa.PublicationOrigin.transfer_preserves_captured_outbox_owner' does not depend on any axioms -/
#guard_msgs in
#print axioms transfer_preserves_captured_outbox_owner

/-- info: 'Causa.PublicationOrigin.fresh_capture_preserves_captured_owner_uniqueness' does not depend on any axioms -/
#guard_msgs in
#print axioms fresh_capture_preserves_captured_owner_uniqueness

/-- info: 'Causa.PublicationOrigin.transfer_preserves_original_owner_and_source' does not depend on any axioms -/
#guard_msgs in
#print axioms transfer_preserves_original_owner_and_source

/-- info: 'Causa.PublicationOrigin.outbox_pruning_preserves_original_owner_and_source' does not depend on any axioms -/
#guard_msgs in
#print axioms outbox_pruning_preserves_original_owner_and_source

/-- info: 'Causa.PublicationOrigin.source_revoke_removes_matching_origin' does not depend on any axioms -/
#guard_msgs in
#print axioms source_revoke_removes_matching_origin

/-- info: 'Causa.PublicationOrigin.source_revoke_preserves_nonmatching_origin' does not depend on any axioms -/
#guard_msgs in
#print axioms source_revoke_preserves_nonmatching_origin

/-- info: 'Causa.PublicationOrigin.source_revoke_removes_matching_outbox_copy' does not depend on any axioms -/
#guard_msgs in
#print axioms source_revoke_removes_matching_outbox_copy

/-- info: 'Causa.PublicationOrigin.source_revoke_removes_captured_outbox_owner' does not depend on any axioms -/
#guard_msgs in
#print axioms source_revoke_removes_captured_outbox_owner

/-- info: 'Causa.PublicationOrigin.source_revoke_preserves_unmatched_outbox_id' does not depend on any axioms -/
#guard_msgs in
#print axioms source_revoke_preserves_unmatched_outbox_id

/-- info: 'Causa.PublicationOrigin.source_revoke_retains_live_facts_and_current_owners' does not depend on any axioms -/
#guard_msgs in
#print axioms source_revoke_retains_live_facts_and_current_owners

/-- info: 'Causa.PublicationOrigin.owner_revoke_removes_matching_origin' does not depend on any axioms -/
#guard_msgs in
#print axioms owner_revoke_removes_matching_origin

/-- info: 'Causa.PublicationOrigin.owner_revoke_preserves_other_owner_origin' does not depend on any axioms -/
#guard_msgs in
#print axioms owner_revoke_preserves_other_owner_origin

/-- info: 'Causa.PublicationOrigin.owner_revoke_removes_matching_outbox_copy' does not depend on any axioms -/
#guard_msgs in
#print axioms owner_revoke_removes_matching_outbox_copy

/-- info: 'Causa.PublicationOrigin.owner_revoke_removes_captured_outbox_owner' does not depend on any axioms -/
#guard_msgs in
#print axioms owner_revoke_removes_captured_outbox_owner

/-- info: 'Causa.PublicationOrigin.owner_revoke_removes_source_free_outbox_copy' does not depend on any axioms -/
#guard_msgs in
#print axioms owner_revoke_removes_source_free_outbox_copy

/-- info: 'Causa.PublicationOrigin.owner_revoke_preserves_unmatched_outbox_id' does not depend on any axioms -/
#guard_msgs in
#print axioms owner_revoke_preserves_unmatched_outbox_id

/-- info: 'Causa.PublicationOrigin.owner_revoke_retains_live_facts_and_current_owners' does not depend on any axioms -/
#guard_msgs in
#print axioms owner_revoke_retains_live_facts_and_current_owners

/-- info: 'Causa.PublicationOrigin.exact_fact_revoke_is_global' does not depend on any axioms -/
#guard_msgs in
#print axioms exact_fact_revoke_is_global

/-- info: 'Causa.PublicationOrigin.exact_fact_revoke_preserves_other_fact' does not depend on any axioms -/
#guard_msgs in
#print axioms exact_fact_revoke_preserves_other_fact

/-- info: 'Causa.PublicationOrigin.intake_eligibility_requires_matching_origin_and_no_witness' does not depend on any axioms -/
#guard_msgs in
#print axioms intake_eligibility_requires_matching_origin_and_no_witness

/-- info: 'Causa.PublicationOrigin.missing_origin_fails_closed' does not depend on any axioms -/
#guard_msgs in
#print axioms missing_origin_fails_closed

/-- info: 'Causa.PublicationOrigin.hard_delete_witness_fails_closed' does not depend on any axioms -/
#guard_msgs in
#print axioms hard_delete_witness_fails_closed

/-- info: 'Causa.PublicationOrigin.backlog_reoffer_uses_intake_eligibility' does not depend on any axioms -/
#guard_msgs in
#print axioms backlog_reoffer_uses_intake_eligibility

/-- info: 'Causa.PublicationOrigin.replay_does_not_restore_revoked_origin' does not depend on any axioms -/
#guard_msgs in
#print axioms replay_does_not_restore_revoked_origin

/-- info: 'Causa.PublicationOrigin.no_step_restores_revoked_origin' does not depend on any axioms -/
#guard_msgs in
#print axioms no_step_restores_revoked_origin

/-- info: 'Causa.PublicationOrigin.source_revocation_does_not_enable_same_fact_recapture' does not depend on any axioms -/
#guard_msgs in
#print axioms source_revocation_does_not_enable_same_fact_recapture

/-- info: 'Causa.PublicationOrigin.fresh_fact_can_reuse_revoked_source' does not depend on any axioms -/
#guard_msgs in
#print axioms fresh_fact_can_reuse_revoked_source

/-- info: 'Causa.PublicationOrigin.backfill_requires_surviving_evidence' does not depend on any axioms -/
#guard_msgs in
#print axioms backfill_requires_surviving_evidence

/-- info: 'Causa.PublicationOrigin.missing_backfill_evidence_fails_closed' does not depend on any axioms -/
#guard_msgs in
#print axioms missing_backfill_evidence_fails_closed

/-- info: 'Causa.PublicationOrigin.missing_owner_or_source_metadata_fails_closed' does not depend on any axioms -/
#guard_msgs in
#print axioms missing_owner_or_source_metadata_fails_closed

/-- info: 'Causa.PublicationOrigin.backfill_after_transfer_uses_captured_original_owner' does not depend on any axioms -/
#guard_msgs in
#print axioms backfill_after_transfer_uses_captured_original_owner

/-- info: 'Causa.PublicationOrigin.backfill_accepts_known_source_absence' does not depend on any axioms -/
#guard_msgs in
#print axioms backfill_accepts_known_source_absence

/-- info: 'Causa.PublicationOrigin.missing_known_source_evidence_fails_closed' does not depend on any axioms -/
#guard_msgs in
#print axioms missing_known_source_evidence_fails_closed

end Causa.PublicationOrigin
