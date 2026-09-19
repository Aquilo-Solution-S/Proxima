import Causa.HostStateLifecycle

namespace Causa.HostStateLifecycle

/-- info: 'Causa.HostStateLifecycle.SourceScopeId.ext_of_token' does not depend on any axioms -/
#guard_msgs in
#print axioms SourceScopeId.ext_of_token

/-- info: 'Causa.HostStateLifecycle.coverage_is_complete_and_disjoint' does not depend on any axioms -/
#guard_msgs in
#print axioms coverage_is_complete_and_disjoint

/-- info: 'Causa.HostStateLifecycle.exact_facts_scope_is_distinct' does not depend on any axioms -/
#guard_msgs in
#print axioms exact_facts_scope_is_distinct

/-- info: 'Causa.HostStateLifecycle.receipt_binds_participant_owner_scope_and_tables' does not depend on any axioms -/
#guard_msgs in
#print axioms receipt_binds_participant_owner_scope_and_tables

/-- info: 'Causa.HostStateLifecycle.receipt_binds_typed_selection' does not depend on any axioms -/
#guard_msgs in
#print axioms receipt_binds_typed_selection

/-- info: 'Causa.HostStateLifecycle.selection_mismatch_invalidates_receipt' does not depend on any axioms -/
#guard_msgs in
#print axioms selection_mismatch_invalidates_receipt

/-- info: 'Causa.HostStateLifecycle.exact_facts_receipt_has_no_original_copy_selection' does not depend on any axioms -/
#guard_msgs in
#print axioms exact_facts_receipt_has_no_original_copy_selection

/-- info: 'Causa.HostStateLifecycle.exact_facts_request_and_receipt_have_no_original_copy_selection' does not depend on any axioms -/
#guard_msgs in
#print axioms exact_facts_request_and_receipt_have_no_original_copy_selection

/-- info: 'Causa.HostStateLifecycle.retained_source_surface_has_zero_counts' depends on axioms: [propext] -/
#guard_msgs in
#print axioms retained_source_surface_has_zero_counts

/-- info: 'Causa.HostStateLifecycle.retained_exact_fact_surface_has_zero_counts' depends on axioms: [propext] -/
#guard_msgs in
#print axioms retained_exact_fact_surface_has_zero_counts

/-- info: 'Causa.HostStateLifecycle.rejected_erase_preserves_core_and_host' does not depend on any axioms -/
#guard_msgs in
#print axioms rejected_erase_preserves_core_and_host

/-- info: 'Causa.HostStateLifecycle.committed_erase_binds_owner_and_scope' does not depend on any axioms -/
#guard_msgs in
#print axioms committed_erase_binds_owner_and_scope

/-- info: 'Causa.HostStateLifecycle.completed_export_is_owner_bound' does not depend on any axioms -/
#guard_msgs in
#print axioms completed_export_is_owner_bound

/-- info: 'Causa.HostStateLifecycle.completed_export_contains_only_declared_surfaces' depends on axioms: [propext, Quot.sound] -/
#guard_msgs in
#print axioms completed_export_contains_only_declared_surfaces

/-- info: 'Causa.HostStateLifecycle.example_registration_has_complete_disjoint_coverage' depends on axioms: [propext] -/
#guard_msgs in
#print axioms example_registration_has_complete_disjoint_coverage

/-- info: 'Causa.HostStateLifecycle.example_configuration_has_explicit_source_retention_and_export_exclusion' depends on axioms: [propext] -/
#guard_msgs in
#print axioms example_configuration_has_explicit_source_retention_and_export_exclusion

end Causa.HostStateLifecycle
