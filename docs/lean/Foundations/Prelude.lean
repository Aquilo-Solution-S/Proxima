/-
Proxima Foundations — Prelude

Kernel-wide primitives with no dependencies of their own:
- The minimal `Set` definition (avoids Mathlib).
- Opaque value Types whose representation is deferred but whose
  identity earns a kernel slot (`Instant`, `Text`).
- Forward-referenced Types needed across domain files.

Every file in `Foundations/` imports this module. Nothing else
imports anything heavier here — keep the prelude clean.

This kernel is the source of truth for Proxima's domainless
invariants. The prose docs (`docs/*.md`) are rationale and
commentary; where they disagree with the kernel, the kernel wins
until renegotiated in writing. Spec-mode Lean: every primitive is
an `axiom` or closed `inductive`; there are no proof obligations.
-/

namespace Proxima

-- ============================================================
-- Minimal Set primitive (avoids a Mathlib dependency).
-- Equivalent to Mathlib's `Set α := α → Prop`.
-- ============================================================

def Set (α : Type) : Type := α → Prop

instance {α : Type} : Membership α (Set α) := ⟨fun s a => s a⟩

-- ============================================================
-- Opaque value Types
-- ============================================================

/-- Time-point identity. Kept opaque; only `≤` is exposed. Two
    distinct time accessors exist on Event (`observed_at`,
    `occurred_at` — doc 01 §Properties of an Event); the kernel
    commits to the distinction, not to clock structure. -/
axiom Instant : Type
axiom Instant.le : Instant → Instant → Prop
instance : LE Instant := ⟨Instant.le⟩

/-- Authored cognitive text on Abstractions and Perspectives
    (doc 02 §The Core Entity). Opaque: the kernel commits to its
    existence and immutability, not its encoding. Facts have no
    stored text — they render from typed payload on demand
    (doc 03 §Renderer), which is engine behavior, not kernel. -/
axiom Text : Type

end Proxima
