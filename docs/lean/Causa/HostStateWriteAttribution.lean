import Causa.HostStateMaintenance

/-
Write attribution is a label carried alongside the existing host-state permit.
The source constructor is supplied only after the existing ordinary or
maintenance admission path; this model neither proves that runtime boundary
nor changes the permit's participant, table, owner, or authority scope.
-/

namespace Causa.HostStateWriteAttribution

open Causa
open Causa.HostStateMaintenance

inductive AdmittedWriteSource where
  | ownerAuthorized (principal : OwnerRef)
  | maintenance

inductive WriteOrigin where
  | ownerAuthorized (principal : OwnerRef)
  | maintenance

/-- This carrier begins after the existing ordinary authorization decision.
    It records the trusted context principal, not a new authorization rule. -/
structure AlreadyAuthorizedContext where
  principal : OwnerRef

def sourceOrigin : AdmittedWriteSource → WriteOrigin
  | .ownerAuthorized principal => .ownerAuthorized principal
  | .maintenance => .maintenance

structure AttributedPermit where
  permit : Permit
  origin : WriteOrigin

def stampPermit {Command : Type} (permit : Permit) (source : AdmittedWriteSource)
    (_targetOwner : OwnerRef) (_command : Command) : AttributedPermit :=
  ⟨permit, sourceOrigin source⟩

def stampOrdinaryPermit {Command : Type} (permit : Permit)
    (context : AlreadyAuthorizedContext) (_targetOwner : OwnerRef) (_command : Command) :
    AttributedPermit :=
  ⟨permit, .ownerAuthorized context.principal⟩

theorem ordinary_stamp_uses_context_principal
    (permit : Permit) (context : AlreadyAuthorizedContext)
    (targetOwner : OwnerRef) (command : Command) :
    (stampOrdinaryPermit permit context targetOwner command).origin =
      .ownerAuthorized context.principal := rfl

theorem maintenance_stamp_has_no_ordinary_principal
    (permit : Permit) (principal targetOwner : OwnerRef) (command : Command) :
    (stampPermit permit .maintenance targetOwner command).origin ≠
      .ownerAuthorized principal := by
  intro impossible
  cases impossible

theorem command_target_and_bytes_cannot_change_attribution
    {Command : Type} (permit : Permit) (source : AdmittedWriteSource)
    (target₁ target₂ : OwnerRef) (bytes₁ bytes₂ : Command) :
    (stampPermit permit source target₁ bytes₁).origin =
      (stampPermit permit source target₂ bytes₂).origin := rfl

theorem attribution_preserves_existing_permit
    {Command : Type} (permit : Permit) (source : AdmittedWriteSource)
    (target : OwnerRef) (command : Command) :
  (stampPermit permit source target command).permit = permit := rfl

theorem ordinary_attribution_preserves_existing_permit
    {Command : Type} (permit : Permit) (context : AlreadyAuthorizedContext)
    (target : OwnerRef) (command : Command) :
    (stampOrdinaryPermit permit context target command).permit = permit := rfl

end Causa.HostStateWriteAttribution
