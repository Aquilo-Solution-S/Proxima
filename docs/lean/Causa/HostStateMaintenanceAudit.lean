import Causa.HostStateMaintenance

namespace Causa.HostStateMaintenance

/-- info: 'Causa.HostStateMaintenance.minted_from_registration' depends on axioms: [propext] -/
#guard_msgs in
#print axioms minted_from_registration

/-- info: 'Causa.HostStateMaintenance.minted_registration_valid' depends on axioms: [propext] -/
#guard_msgs in
#print axioms minted_registration_valid

/-- info: 'Causa.HostStateMaintenance.no_registration_no_capability' depends on axioms: [propext] -/
#guard_msgs in
#print axioms no_registration_no_capability

/-- info: 'Causa.HostStateMaintenance.foreign_system_witness_rejected' depends on axioms: [propext] -/
#guard_msgs in
#print axioms foreign_system_witness_rejected

/-- info: 'Causa.HostStateMaintenance.invalid_registration_rejected' depends on axioms: [propext] -/
#guard_msgs in
#print axioms invalid_registration_rejected

/-- info: 'Causa.HostStateMaintenance.duplicate_registration_rejected' depends on axioms: [propext] -/
#guard_msgs in
#print axioms duplicate_registration_rejected

/-- info: 'Causa.HostStateMaintenance.authorization_sound' depends on axioms: [propext] -/
#guard_msgs in
#print axioms authorization_sound

/-- info: 'Causa.HostStateMaintenance.foreign_engine_rejected' depends on axioms: [propext] -/
#guard_msgs in
#print axioms foreign_engine_rejected

/-- info: 'Causa.HostStateMaintenance.foreign_participant_rejected' depends on axioms: [propext] -/
#guard_msgs in
#print axioms foreign_participant_rejected

/-- info: 'Causa.HostStateMaintenance.registration_change_rejected' depends on axioms: [propext] -/
#guard_msgs in
#print axioms registration_change_rejected

/-- info: 'Causa.HostStateMaintenance.empty_tables_rejected' depends on axioms: [propext, Quot.sound] -/
#guard_msgs in
#print axioms empty_tables_rejected

/-- info: 'Causa.HostStateMaintenance.duplicate_tables_rejected' depends on axioms: [propext] -/
#guard_msgs in
#print axioms duplicate_tables_rejected

/-- info: 'Causa.HostStateMaintenance.undeclared_table_rejected' depends on axioms: [propext] -/
#guard_msgs in
#print axioms undeclared_table_rejected

/-- info: 'Causa.HostStateMaintenance.authorized_tables_are_state_surfaces' depends on axioms: [propext] -/
#guard_msgs in
#print axioms authorized_tables_are_state_surfaces

/-- info: 'Causa.HostStateMaintenance.non_state_surface_rejected' depends on axioms: [propext] -/
#guard_msgs in
#print axioms non_state_surface_rejected

/-- info: 'Causa.HostStateMaintenance.payload_owner_is_command_owner' depends on axioms: [propext] -/
#guard_msgs in
#print axioms payload_owner_is_command_owner

/-- info: 'Causa.HostStateMaintenance.unit_owner_is_preserved' depends on axioms: [propext, Quot.sound] -/
#guard_msgs in
#print axioms unit_owner_is_preserved

/-- info: 'Causa.HostStateMaintenance.foreign_unit_owner_rejected' depends on axioms: [propext, Quot.sound] -/
#guard_msgs in
#print axioms foreign_unit_owner_rejected

/-- info: 'Causa.HostStateMaintenance.maintenance_preserves_cognitive_state' depends on axioms: [propext, Quot.sound] -/
#guard_msgs in
#print axioms maintenance_preserves_cognitive_state

/-- info: 'Causa.HostStateMaintenance.maintenance_preserves_ordinary_write_authority' depends on axioms: [propext, Quot.sound] -/
#guard_msgs in
#print axioms maintenance_preserves_ordinary_write_authority

/-- info: 'Causa.HostStateMaintenance.every_owner_supported' depends on axioms: [propext, Quot.sound] -/
#guard_msgs in
#print axioms every_owner_supported

/-- info: 'Causa.HostStateMaintenance.every_owner_step' depends on axioms: [propext, Quot.sound] -/
#guard_msgs in
#print axioms every_owner_step

/-- info: 'Causa.HostStateMaintenance.maintenance_without_fact_write' depends on axioms: [propext, Quot.sound] -/
#guard_msgs in
#print axioms maintenance_without_fact_write

/-- info: 'Causa.HostStateMaintenance.distinct_users_exist' does not depend on any axioms -/
#guard_msgs in
#print axioms distinct_users_exist

end Causa.HostStateMaintenance
