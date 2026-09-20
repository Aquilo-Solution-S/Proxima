import Causa.Authorization

/-
Host-state maintenance is a separate authority domain. A boot-held witness
can mint a capability for the participant actually registered on that engine;
its commands may name either owner kind without granting cognitive writes.

Runtime residuals: fresh engine bindings, private Rust constructors, descriptor
capture from the actual storage registration, a shared owner lifecycle fence
against exclusive erasure, and participant SQL honoring the declared surfaces
and stamped owner. Capabilities stay in host code, never FlavorServices or
user-facing authoring handlers. Scope is a participant and its tables, not a
whitelist distinguishing that participant's command types. No SQL sandbox is
claimed.
-/

namespace Causa.HostStateMaintenance

structure EngineBinding where
  token : String
  deriving DecidableEq

structure ParticipantId where
  token : String
  deriving DecidableEq

structure StateSurface where
  name : String
  deriving DecidableEq

structure Registration where
  participant : ParticipantId
  tables : List StateSurface
  deriving DecidableEq

structure Boot where
  engine : EngineBinding
  stateSurfaces : List StateSurface
  registered : Option Registration

def RegistrationValid (boot : Boot) : Prop :=
  match boot.registered with
  | none => True
  | some registration =>
      registration.tables.Nodup ∧
        ∀ table ∈ registration.tables, table ∈ boot.stateSurfaces

instance (boot : Boot) : Decidable (RegistrationValid boot) := by
  unfold RegistrationValid
  split <;> infer_instance

structure SystemAuthority where
  private mk ::
  engine : EngineBinding

def Boot.systemAuthority (boot : Boot) : SystemAuthority :=
  ⟨boot.engine⟩

structure Capability where
  private mk ::
  engine : EngineBinding
  registration : Registration

def mint (boot : Boot) (system : SystemAuthority) : Option Capability :=
  if system.engine = boot.engine then
    match boot.registered with
    | none => none
    | some registration =>
        if RegistrationValid boot then
          some ⟨boot.engine, registration⟩
        else none
  else none

structure Command where
  participant : ParticipantId
  tables : List StateSurface
  owner : OwnerRef

def InScope (boot : Boot) (capability : Capability) (command : Command) : Prop :=
  capability.engine = boot.engine ∧
  boot.registered = some capability.registration ∧
  command.participant = capability.registration.participant ∧
  command.tables ≠ [] ∧
  command.tables.Nodup ∧
  (∀ table ∈ command.tables, table ∈ capability.registration.tables) ∧
  ∀ table ∈ command.tables, table ∈ boot.stateSurfaces

instance (boot : Boot) (capability : Capability) (command : Command) :
    Decidable (InScope boot capability command) := by
  unfold InScope
  infer_instance

structure Permit where
  private mk ::
  engine : EngineBinding
  participant : ParticipantId
  tables : List StateSurface
  owner : OwnerRef

def authorize (boot : Boot) (capability : Capability) (command : Command) :
    Option Permit :=
  if InScope boot capability command then
    some ⟨boot.engine, command.participant, command.tables, command.owner⟩
  else none

theorem minted_from_registration {boot : Boot} {system : SystemAuthority}
    {capability : Capability} (h : mint boot system = some capability) :
    system.engine = boot.engine ∧ capability.engine = boot.engine ∧
      boot.registered = some capability.registration := by
  unfold mint at h
  split at h
  next bound =>
    cases registered : boot.registered with
    | none => simp [registered] at h
    | some registration =>
      by_cases valid : RegistrationValid boot
      · simp [registered, valid] at h
        subst capability
        exact ⟨bound, rfl, rfl⟩
      · simp [registered, valid] at h
  next => contradiction

theorem minted_registration_valid {boot : Boot} {system : SystemAuthority}
    {capability : Capability} (h : mint boot system = some capability) :
    RegistrationValid boot := by
  unfold mint at h
  split at h
  next bound =>
    cases registered : boot.registered with
    | none => simp [registered] at h
    | some registration =>
      by_cases valid : RegistrationValid boot
      · exact valid
      · simp [registered, valid] at h
  next => contradiction

theorem no_registration_no_capability (boot : Boot) (system : SystemAuthority)
    (h : boot.registered = none) : mint boot system = none := by
  simp [mint, h]

theorem foreign_system_witness_rejected (boot : Boot) (system : SystemAuthority)
    (h : system.engine ≠ boot.engine) : mint boot system = none := by
  simp [mint, h]

theorem invalid_registration_rejected (boot : Boot) (system : SystemAuthority)
    (invalid : ¬ RegistrationValid boot) : mint boot system = none := by
  by_cases same : system.engine = boot.engine
  · cases registered : boot.registered with
    | none => simp [mint, same, registered]
    | some registration =>
      by_cases valid : RegistrationValid boot
      · contradiction
      · simp [mint, same, registered, valid]
  · simp [mint, same]

theorem duplicate_registration_rejected (boot : Boot) (system : SystemAuthority)
    (registration : Registration) (registered : boot.registered = some registration)
    (duplicates : ¬ registration.tables.Nodup) : mint boot system = none := by
  apply invalid_registration_rejected
  intro valid
  have checked : registration.tables.Nodup ∧
      ∀ table ∈ registration.tables, table ∈ boot.stateSurfaces := by
    simpa [RegistrationValid, registered] using valid
  exact duplicates checked.1

theorem authorization_sound {boot : Boot} {capability : Capability}
    {command : Command} {permit : Permit}
    (h : authorize boot capability command = some permit) :
    InScope boot capability command ∧ permit.engine = boot.engine ∧
      permit.participant = command.participant ∧ permit.tables = command.tables ∧
      permit.owner = command.owner := by
  unfold authorize at h
  split at h
  next scope =>
    cases h
    exact ⟨scope, rfl, rfl, rfl, rfl⟩
  next => contradiction

theorem foreign_engine_rejected (boot : Boot) (capability : Capability)
    (command : Command) (h : capability.engine ≠ boot.engine) :
    authorize boot capability command = none := by
  simp [authorize, InScope, h]

theorem foreign_participant_rejected (boot : Boot) (capability : Capability)
    (command : Command)
    (h : command.participant ≠ capability.registration.participant) :
    authorize boot capability command = none := by
  simp [authorize, InScope, h]

theorem registration_change_rejected (boot : Boot) (capability : Capability)
    (command : Command) (h : boot.registered ≠ some capability.registration) :
    authorize boot capability command = none := by
  simp [authorize, InScope, h]

theorem empty_tables_rejected (boot : Boot) (capability : Capability)
    (command : Command) (h : command.tables = []) :
    authorize boot capability command = none := by
  simp [authorize, InScope, h]

theorem duplicate_tables_rejected (boot : Boot) (capability : Capability)
    (command : Command) (duplicates : ¬ command.tables.Nodup) :
    authorize boot capability command = none := by
  have outside : ¬ InScope boot capability command := by
    intro h
    exact duplicates h.2.2.2.2.1
  simp [authorize, outside]

theorem undeclared_table_rejected (boot : Boot) (capability : Capability)
    (command : Command) (table : StateSurface) (requested : table ∈ command.tables)
    (undeclared : table ∉ capability.registration.tables) :
    authorize boot capability command = none := by
  have outside : ¬ InScope boot capability command := by
    intro h
    exact undeclared (h.2.2.2.2.2.1 table requested)
  simp [authorize, outside]

theorem authorized_tables_are_state_surfaces {boot : Boot} {capability : Capability}
    {command : Command} {permit : Permit}
    (h : authorize boot capability command = some permit) :
    ∀ table ∈ permit.tables, table ∈ boot.stateSurfaces := by
  obtain ⟨scope, _, _, tables, _⟩ := authorization_sound h
  intro table present
  rw [tables] at present
  exact scope.2.2.2.2.2.2 table present

theorem non_state_surface_rejected (boot : Boot) (capability : Capability)
    (command : Command) (table : StateSurface) (requested : table ∈ command.tables)
    (undeclared : table ∉ boot.stateSurfaces) :
    authorize boot capability command = none := by
  have outside : ¬ InScope boot capability command := by
    intro h
    exact undeclared (h.2.2.2.2.2.2 table requested)
  simp [authorize, outside]

/-- The participant must check its payload's owner against the stamped permit.
    Command authorization alone does not establish payload agreement. -/
def PayloadAllowed (permit : Permit) (payloadOwner : OwnerRef) : Prop :=
  payloadOwner = permit.owner

theorem payload_owner_is_command_owner {boot : Boot} {capability : Capability}
    {command : Command} {permit : Permit} {payloadOwner : OwnerRef}
    (h : authorize boot capability command = some permit)
    (payload : PayloadAllowed permit payloadOwner) :
    payloadOwner = command.owner :=
  payload.trans (authorization_sound h).2.2.2.2

/-- Host state is outside cognitive state and the ordinary owner-role map.
    These are state parameters, not new kernel tables. -/
structure State (Cognitive Host : Type) where
  owners : OwnerState
  cognitive : Cognitive
  host : Host

inductive Step {Cognitive Host : Type} (boot : Boot) (capability : Capability)
    (unitOwner : OwnerRef) (command : Command) (payloadOwner : OwnerRef) :
    State Cognitive Host → State Cognitive Host → Prop where
  | execute (before : State Cognitive Host) (afterHost : Host) (permit : Permit)
    (authorized : authorize boot capability command = some permit)
    (unitScope : command.owner = unitOwner)
    (payload : PayloadAllowed permit payloadOwner) :
    Step boot capability unitOwner command payloadOwner before {before with host := afterHost}

theorem unit_owner_is_preserved {Cognitive Host : Type}
    {boot : Boot} {capability : Capability} {unitOwner : OwnerRef}
    {command : Command} {payloadOwner : OwnerRef} {before after : State Cognitive Host}
    (h : Step boot capability unitOwner command payloadOwner before after) :
    command.owner = unitOwner ∧ payloadOwner = unitOwner := by
  cases h with
  | execute _ _ authorized unitScope payload =>
    exact ⟨unitScope, (payload_owner_is_command_owner authorized payload).trans unitScope⟩

theorem foreign_unit_owner_rejected {Cognitive Host : Type}
    {boot : Boot} {capability : Capability} {unitOwner : OwnerRef}
    {command : Command} {payloadOwner : OwnerRef} {before after : State Cognitive Host}
    (different : command.owner ≠ unitOwner) :
    ¬ Step boot capability unitOwner command payloadOwner before after := by
  intro h
  exact different (unit_owner_is_preserved h).1

theorem maintenance_preserves_cognitive_state {Cognitive Host : Type}
    {boot : Boot} {capability : Capability} {unitOwner : OwnerRef}
    {command : Command} {payloadOwner : OwnerRef}
    {before after : State Cognitive Host}
    (h : Step boot capability unitOwner command payloadOwner before after) :
    after.cognitive = before.cognitive := by
  cases h
  rfl

theorem maintenance_preserves_ordinary_write_authority {Cognitive Host : Type}
    {boot : Boot} {capability : Capability} {unitOwner : OwnerRef}
    {command : Command} {payloadOwner : OwnerRef}
    {before after : State Cognitive Host}
    (h : Step boot capability unitOwner command payloadOwner before after)
    (requester : User) (owner : OwnerRef) (kind : AccessKind) :
    may_write_in after.owners requester owner kind ↔
      may_write_in before.owners requester owner kind := by
  cases h
  exact Iff.rfl

theorem every_owner_supported (engine : EngineBinding) (participant : ParticipantId)
    (table : StateSurface) (owner : OwnerRef) :
    let registration : Registration := ⟨participant, [table]⟩
    let boot : Boot := ⟨engine, [table], some registration⟩
    let command : Command := ⟨participant, [table], owner⟩
    RegistrationValid boot ∧ ∃ capability permit,
      mint boot boot.systemAuthority = some capability ∧
      authorize boot capability command = some permit ∧ permit.owner = owner := by
  dsimp
  constructor
  · constructor
    · simp
    · intro surface present
      exact present
  · refine ⟨⟨engine, ⟨participant, [table]⟩⟩,
      ⟨engine, participant, [table], owner⟩, ?_, ?_, rfl⟩
    · simp [mint, Boot.systemAuthority, RegistrationValid]
    · simp [authorize, InScope]

theorem every_owner_step {Cognitive Host : Type} (engine : EngineBinding)
    (participant : ParticipantId) (table : StateSurface) (owner : OwnerRef)
    (before : State Cognitive Host) (afterHost : Host) :
    ∃ boot capability command, command.owner = owner ∧
      Step boot capability owner command owner before {before with host := afterHost} := by
  obtain ⟨_, capability, permit, _, authorized, stamped⟩ :=
    every_owner_supported engine participant table owner
  refine ⟨⟨engine, [table], some ⟨participant, [table]⟩⟩, capability,
    ⟨participant, [table], owner⟩, rfl, ?_⟩
  exact .execute before afterHost permit authorized rfl stamped.symm

/-- A successful maintenance admission is compatible with denial of Fact writes
    on that same personal owner. The premise is inhabited by distinct users. -/
theorem maintenance_without_fact_write (engine : EngineBinding)
    (participant : ParticipantId) (table : StateSurface) (owners : OwnerState)
    (owner requester : User) (distinct : requester ≠ owner) :
    (let registration : Registration := ⟨participant, [table]⟩
     let boot : Boot := ⟨engine, [table], some registration⟩
     let command : Command := ⟨participant, [table], .personal owner⟩
     ∃ capability permit, mint boot boot.systemAuthority = some capability ∧
       authorize boot capability command = some permit ∧
       permit.owner = .personal owner) ∧
    ¬ may_write_in owners requester (.personal owner) .fact := by
  refine ⟨(every_owner_supported engine participant table (.personal owner)).2, ?_⟩
  apply (owner_state_non_member_denied owners requester (.personal owner) .fact ?_).2
  rw [owners.personal_resolves]
  exact Owner.ofUser_other distinct

theorem distinct_users_exist : ∃ owner requester : User, requester ≠ owner := by
  refine ⟨User.ofToken "maintenance-owner", User.ofToken "maintenance-worker", ?_⟩
  intro equal
  have impossible := User.ofToken_inj equal
  exact (by decide : ("maintenance-worker" : String) ≠ "maintenance-owner") impossible

end Causa.HostStateMaintenance
