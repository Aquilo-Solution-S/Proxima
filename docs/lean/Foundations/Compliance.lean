/-
Proxima Foundations — Compliance

The ONLY delete path in the system (doc 13; ST-13). Two lifecycles,
strictly separated (doc 13 §Contract boundary):

  - cognitive:   append-only; Facts immutable; A/P/Goals supersede;
  - compliance:  out-of-band admin operation; may hard-delete
                 substrate rows; scoped to one Owner or one
                 Owner-scoped source object; admin/controller-
                 authored, never operator-authored; surfaced through
                 the admin protocol, never as a Memory mutation.

CO-14 — refusal is a valid compliance result, not a substrate
failure (a lawful/retention hold may block erasure).

Excluded as engine/controller mechanics (recorded in COVERAGE.md):
audit row content bounds (CO-21..26 — ids/timestamps/counts, never
payloads), export serialization (CO-11), external side effects
(CO-30..33 — already-sent emails are not rolled back; downstream
cleanup is a controller obligation), owner-policy defaults
(CO-46..52), GDPR article mappings (CO-53..58).
-/

import Foundations.Prelude
import Foundations.Owner
import Foundations.Identity
import Foundations.Memory
import Foundations.Goals
import Foundations.Edges

namespace Proxima

-- ============================================================
-- Operations and outcomes (doc 13 §Operations, §Outcomes)
-- ============================================================

/-- The closed compliance surface. CO-3 — the constructor shapes ARE
    the scope rule: nothing broader than one Owner or one
    Owner-scoped source object is expressible. -/
inductive ComplianceOp where
  | DeleteOwner       (o : Owner)
  | DeleteSourceScope (o : Owner) (s : SourceId)
  | PauseOwner        (o : Owner)
  | ResumeOwner       (o : Owner)
  | ExportOwner       (o : Owner)

/-- CO-12/13/23 — outcomes. CO-14: `Refused` is a valid result. -/
inductive ComplianceOutcome where
  | Completed
  | Refused
  | NotFound
  | Unauthorized
  deriving DecidableEq, Repr

-- ============================================================
-- Suppression list (doc 13 §Suppression list)
-- ============================================================

/-- CO-15/16/20 — a suppression entry retains ONLY the opaque
    content-derived idempotency key (EventId) plus operation
    metadata (deletion timestamp, operation id — engine-level). The
    accessor shape is the PII guard: no natural-person identifier —
    and per the minimization pass, NO Owner — is reachable from a
    suppression entry (doc 13 §Suppression list retains "opaque
    idempotency key only"; owner is reachable only via operation id →
    audit row, an audit-log concern). EventIds are content-derived
    and opaque by construction (doc 01 §Idempotency-key constraint);
    a non-opaque key would itself become PII surviving deletion. -/
axiom SuppressionEntry : Type
axiom suppression_key : SuppressionEntry → EventId

/-- CO-17/18 — source ingest checks suppression before dedup: a
    suppressed event id produces no Fact. Global by key — the entry
    carries no Owner, and EventId is already (source, owner, payload)-
    determined (ES-4), so key-matching IS scope-matching. Rejection
    is a no-op `Suppressed`, no retry pressure. CO-19 — entries are
    retained indefinitely; survival of `delete_owner` is structural
    (see below). -/
axiom suppression_blocks_reingest :
  ∀ (s : SuppressionEntry) (m : Memory) (e : Event),
    memory_source_event m = some e →
    event_id e ≠ suppression_key s

-- ============================================================
-- Erasure semantics (doc 13 §Operations)
-- ============================================================

/-- What `delete_owner` removes vs retains (CO-7), stated as the
    survival predicate over the post-erasure substrate. Erasure
    removes owner-scoped memories, goals, edges, sidecars,
    embeddings, source-batch payloads, invocation caches — and
    RETAINS suppression entries and audit rows.

    Spec-mode encoding: `erased o` marks an Owner whose erasure
    completed; the survivor axioms state what may still exist for an
    erased Owner. The kernel does not model the substrate-row store
    itself, so removal is expressed through its observable face:
    nothing cognitive remains reachable for an erased Owner. -/
axiom erased : Owner → Prop

/-- CO-7'a — no cognitive entity survives its Owner's erasure. The
    Edge conjunct is PROVED below (an edge's owner is its endpoints'
    owner; no endpoint survives) — minimization pass. -/
axiom erasure_removes_cognitive :
  ∀ o : Owner, erased o →
    (∀ m : Memory, memory_owner m ≠ o) ∧
    (∀ g : Goal,   goal_owner g   ≠ o)

/-- CO-7'a, Edge face — THEOREM from single-owner edge scope. -/
theorem erasure_removes_edges :
    ∀ o : Owner, erased o → ∀ e : Edge, edge_owner e ≠ o := by
  intro o ho e he
  obtain ⟨hm, hg⟩ := erasure_removes_cognitive o ho
  have hscope := (edge_scope_single_owner e).1
  cases hsrc : edge_source e with
  | memory ms =>
      rw [hsrc] at hscope
      exact hm ms (by rw [show NodeRef.owner (.memory ms) = memory_owner ms from rfl] at hscope; rw [hscope, he])
  | goal g =>
      rw [hsrc] at hscope
      exact hg g (by rw [show NodeRef.owner (.goal g) = goal_owner g from rfl] at hscope; rw [hscope, he])

/- CO-7'b / CO-19 / CO-29 — suppression SURVIVES erasure, carried
    structurally: `erasure_removes_cognitive` does not range over
    SuppressionEntry, and `suppression_blocks_reingest` above is
    deliberately NOT conditioned on `¬ erased o` — the re-ingest
    guard keeps holding for an erased Owner. That unconditional
    quantification is the survival invariant; an explicit axiom
    would be a tautology in this timeless model. Audit-row retention
    (CO-21..29) is an engine-table concern → COVERAGE.md exclusion.
    The kernel commits to the suppression face because it is what
    makes erasure final against silent re-ingest. -/

-- ============================================================
-- Pause (doc 13 §Operations)
-- ============================================================

/-- CO-9 — pause stops FUTURE operator dispatch and wake execution;
    reads and export remain available. CO-10 — resume clears it.
    Kernel face: `paused` is a state predicate; the dispatch gate is
    that no personality-authored memory is created for a paused
    Owner — expressed as creation-time guard in the engine, recorded
    here as the predicate the engine must consult. Existing memories
    are untouched (append-only; pause is not erasure). -/
axiom paused : Owner → Prop

end Proxima
