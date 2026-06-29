/-
Causa — Wake (the self-organizing organism)

The wake loop is Fact → Wake → Action → Fact. Every node already exists:
a Fact is a `Memory` with kind `.Fact`; a wake entry is an ARMED `Goal` (a
Goal carrying `WakeConfig`, Goals.lean — "Goals are wake entries when
configured to wake"); an Action's record is a Fact again. So wake adds NO new
entity kind and — like `Causa.Flavor` — NO new axiom. The "self-organizing
organism" is a THEOREM of the existing kernel, not a primitive.

A `Firing` is one wake step: an armed Goal reacts to a trigger Fact and the
execution actor (`actor`, modeled as `User`: human, configured Agent, service
actor) uses its server-resolved role bundle and the Goal's `WakeConfig` to emit
Facts. Every safety property is a STRUCTURAL FIELD of `Firing`, never an axiom —
the same discipline as `Role.write_le_read` and `Causa.Flavor`:

  - W1 closure        — emissions are Facts (`each_fact`); the loop never leaves
                        the ontology (the ToolCall abstraction returns a Fact).
  - W2 no-escalation  — emissions are bounded by the actor's GRANTED write
                        authority (`each_authzd`, reusing `may_write`). The
                        delegation keystone: a self-firing agent cannot widen
                        what the human granted it. An actor with no write role
                        is forced to no-op (`powerless_actor_noops`).
  - W3 read-bounded   — the trigger and every injected memory are within the
                        actor's read authority (`trigger_read`,
                        `each_injected_read`, reusing `may_read`).
  - W4 grounding      — every emitted Fact is created STRICTLY later than its
                        trigger (`each_later`); so causation (`fires`) is
                        well-founded backward, by the SAME clock as N1
                        (Provenance). `organism_grounded`: trace any Fact's
                        causal ancestry and it bottoms out, in finitely many
                        firings, at an UNCAUSED external Fact (the Mail). The
                        loop runs forward forever but is grounded in the world.
  - W5 goal-context   — every emitted Fact is motivated by the firing Goal
                        (`each_motivated`); the organism cannot emit a
                        contextless action. And that motivation edge is FORCED
                        perspectival (N4, `wake_motivation_is_perspectival`):
                        you cannot attribute an action to a goal as an
                        observer-independent fact.
  - W6 tool-bounded   — every invoked Action is admitted by the Goal's
                        `WakeConfig.toolset` (`each_action_allowed`). Concrete
                        tool execution policy remains engine/flavor-side; the
                        kernel pins the allow-list witness.

`#print axioms` (below) is the guarantee: each theorem rests ONLY on axioms the
kernel already trusts — never one named `wake`, because none exists.
-/

import Causa.Goals
import Causa.Edges
import Causa.Authorization
import Causa.Provenance

namespace Causa.Wake

open Causa

-- ============================================================
-- Access-kind of a memory kind (for read-bounding injected context)
-- ============================================================

/-- The access-ladder kind a memory is read under. Facts read as `.fact`,
    Abstractions as `.abstraction`, Perspectives as `.perspective`. -/
def MemoryKind.access : MemoryKind → AccessKind
  | .Fact        => .fact
  | .Abstraction => .abstraction
  | .Perspective => .perspective

-- ============================================================
-- Goal-context: an emitted Fact is causally tied to the firing Goal
-- ============================================================

/-- W5 relation: memory `m` is motivated by `goal` — there is a valid
    Causal-class edge from the Goal to the memory. By `EdgeGoalCausalValidWith`
    (N4) any such edge is necessarily `PerspectiveGoalLink`-authored: a
    perspectival causal claim, never an observer-independent fact. -/
def motivatedByGoal (registry : RelationRegistry) (m : Memory) (goal : Goal) : Prop :=
  ∃ e, EdgeHasClass registry e .Causal ∧
    edge_source e = .goal goal ∧ edge_target e = .memory m

-- ============================================================
-- The firing — one wake step, all safety properties as fields
-- ============================================================

/-- One wake step. The agent (`actor`) reacts to `trigger` on behalf of an
    ARMED `goal` and emits `emitted` (possibly `[]` — "doing nothing is also an
    action"), having injected `injected` as context. The proof fields ARE the
    safety guarantees; none is an axiom. -/
structure Firing where
  registry : RelationRegistry
  actor    : User
  goal     : Goal
  config   : WakeConfig
  trigger  : Fact
  emitted  : List Memory
  injected : List Memory
  invoked  : List Action
  /-- the concrete Goal-owned wake config used for this firing -/
  wake_config        : goal_wake goal = some config
  /-- the Goal is configured to wake -/
  armed              : goalArmed goal
  /-- TARGET 3 — wakes for goals fire ONLY while the goal is ACTIVE. A Paused or
      terminally-closed Goal does not react. This single gate is what makes the
      loop both autonomous (it keeps firing while Active) and self-terminating
      (closing the goal stops it — `terminal_cannot_fire`). -/
  goal_active        : goal_state goal = GoalState.Active
  /-- "human authorized agent at setup" = the agent holds a role in the Goal's
      owner group -/
  actor_member       : goal_owner goal actor ≠ none
  /-- W3: the agent may read the actual trigger Fact's owner -/
  trigger_read       : may_read actor (memory_owner trigger.memory) .fact
  /-- W3: every injected memory is within the agent's read authority -/
  each_injected_read : ∀ m ∈ injected, may_read actor (memory_owner m) (MemoryKind.access (memory_kind m))
  /-- W1: every emission is a Fact -/
  each_fact          : ∀ m ∈ emitted, memory_kind m = .Fact
  /-- W4: every emission is created strictly after the trigger -/
  each_later         : ∀ m ∈ emitted, memory_created_at trigger.memory < memory_created_at m
  /-- W2: every emission is within the agent's GRANTED write authority -/
  each_authzd        : ∀ m ∈ emitted, may_write actor (memory_owner m) .fact
  /-- W5: every emission is motivated by the firing Goal -/
  each_motivated     : ∀ m ∈ emitted, motivatedByGoal registry m goal
  /-- W6: every invoked Action is admitted by the Goal's WakeConfig.toolset -/
  each_action_allowed : ∀ a ∈ invoked, a ∈ config.toolset

-- ============================================================
-- W1–W3, W5 — projections
-- ============================================================

/-- W1 — the loop stays in the ontology: emissions are Facts. -/
theorem wake_emits_facts (fr : Firing) :
    ∀ m ∈ fr.emitted, memory_kind m = .Fact := fr.each_fact

/-- W2 (keystone) — no escalation: an emission is always within the authority
    the human granted the agent. Reuses `may_write`; adds no rule. -/
theorem wake_cannot_escalate (fr : Firing) :
    ∀ m ∈ fr.emitted, may_write fr.actor (memory_owner m) .fact := fr.each_authzd

/-- W2 corollary — an agent granted no write role anywhere is forced to no-op.
    Delegation is total: zero authority ⇒ zero effect. -/
theorem powerless_actor_noops (fr : Firing)
    (h : ∀ o : Owner, ¬ may_write fr.actor o .fact) : fr.emitted = [] := by
  cases hl : fr.emitted with
  | nil => rfl
  | cons m ms =>
    exact absurd (fr.each_authzd m (by rw [hl]; exact List.mem_cons_self m ms))
      (h (memory_owner m))

/-- W3 — the trigger Fact's actual owner is readable by the actor. -/
theorem wake_trigger_readable (fr : Firing) :
    may_read fr.actor (memory_owner fr.trigger.memory) .fact := fr.trigger_read

/-- W6 — every invoked Action is admitted by the Goal-owned wake config. -/
theorem wake_invoked_actions_allowed (fr : Firing) :
    ∀ a ∈ fr.invoked, a ∈ fr.config.toolset := fr.each_action_allowed

/-- W3 — injected context is read-authorized: the agent never injects a memory
    it could not itself read. -/
theorem wake_context_readable (fr : Firing) :
    ∀ m ∈ fr.injected, may_read fr.actor (memory_owner m) (MemoryKind.access (memory_kind m)) :=
  fr.each_injected_read

/-- W5 — every action the organism takes carries goal-context; it cannot emit a
    contextless Fact. This is the formal content of "action without a Goal is
    useless". -/
theorem wake_action_has_goal_context (fr : Firing) :
    ∀ m ∈ fr.emitted, motivatedByGoal fr.registry m fr.goal := fr.each_motivated

/-- W5 (N4) — the goal-context edge is necessarily PERSPECTIVAL: an action's
    attribution to a goal is a perspective-relative causal claim, never an
    observer-independent fact (`causal_goal_edge_perspectival`). -/
theorem wake_motivation_is_perspectival (fr : Firing) (m : Memory) (hm : m ∈ fr.emitted) :
    ∃ e : Edge, edge_authorship e = .PerspectiveGoalLink := by
  obtain ⟨e, hclass, hs, _⟩ := fr.each_motivated m hm
  exact ⟨e, causal_goal_edge_perspectival fr.registry e hclass (Or.inl ⟨fr.goal, hs⟩)⟩

-- ============================================================
-- W4 — the capstone: causation is well-founded (the arrow of time)
-- ============================================================

/-- The causal firing relation between Facts: `fires f g` when some firing was
    triggered by `f` and emitted `g`. Forward in time. -/
def fires (f g : Memory) : Prop :=
  ∃ fr : Firing, fr.trigger.memory = f ∧ g ∈ fr.emitted

/-- `fires` strictly advances `created_at` — a wake cannot emit into its own
    past (`each_later`). -/
theorem fires_advances_time {f g : Memory} (h : fires f g) :
    memory_created_at f < memory_created_at g := by
  obtain ⟨fr, htrig, hmem⟩ := h
  have hlt := fr.each_later g hmem
  rw [htrig] at hlt
  exact hlt

/-- W4 — THE ORGANISM IS GROUNDED. Causation is well-founded: trace any Fact's
    causal ancestry back through firings and it terminates, in finitely many
    steps, at an UNCAUSED Fact — an external input from the world (the Mail).
    THEOREM, no axiom — `fires` strictly decreases the `created_at` instant and
    `<` on `Nat` is well-founded, exactly as N1 grounds provenance. The loop
    runs forward forever, but the arrow of time grounds it. Same clock, mirror
    direction: provenance descends to grounding Facts, causation descends to
    uncaused inputs. -/
theorem organism_grounded : WellFounded fires :=
  Subrelation.wf
    (fun {_ _} h => fires_advances_time h)
    (invImage memory_created_at Nat.lt_wfRel).wf

-- ============================================================
-- Inhabitation 1 — the no-op firing ("doing nothing is an action")
-- ============================================================

/-- The no-op firing: the agent does nothing. Every ∀-over-emitted obligation
    is vacuous, so a wake step exists with no writes and no edges — the
    structure is consistent. This IS "doing nothing is also an Action". -/
def noopFiring (registry : RelationRegistry) (actor : User) (goal : Goal) (config : WakeConfig) (trig : Fact)
    (hcfg : goal_wake goal = some config)
    (harm : goalArmed goal)
    (hactive : goal_state goal = GoalState.Active)
    (hmem : goal_owner goal actor ≠ none)
    (hread : may_read actor (memory_owner trig.memory) .fact) : Firing where
  registry := registry
  actor := actor
  goal := goal
  config := config
  trigger := trig
  emitted := []
  injected := []
  invoked := []
  wake_config := hcfg
  armed := harm
  goal_active := hactive
  actor_member := hmem
  trigger_read := hread
  each_injected_read := by intro m hm; simp at hm
  each_fact := by intro m hm; simp at hm
  each_later := by intro m hm; simp at hm
  each_authzd := by intro m hm; simp at hm
  each_motivated := by intro m hm; simp at hm
  each_action_allowed := by intro a ha; simp at ha

-- ============================================================
-- Inhabitation 2 — a genuine emission with its motivation edge
-- (the loop closes non-vacuously: one Fact in, one Fact out)
-- ============================================================

/-- A build-time relation row admitting Goal→Memory Causal edges. Admits NO
    memory→memory pair, so `masksTightenOnly` is vacuous — it never relaxes the
    F/A/P matrix. -/
def motivationDescriptor (relId : RelationId) : RelationDescriptor where
  id := relId
  relClass := .Causal
  sourceBinding := .Pin
  targetBinding := .Pin
  ownerPolicy := .SourceOwned
  targetAccessPolicy := .None
  endpointAdmitted := fun s t => (∃ g : Goal, s = .goal g) ∧ (∃ m : Memory, t = .memory m)
  masksTightenOnly := by
    intro s _ _ _ h hs _
    obtain ⟨⟨g, hg⟩, _⟩ := h
    rw [hg] at hs
    cases hs
  supersessionSameOwner := by intro h; cases h

/-- A concrete Goal→Fact motivation edge (PerspectiveGoalLink, N4). -/
def motivationEdge (goal : Goal) (m : Memory) (relId : RelationId) (uuid : EdgeUuid) : Edge where
  id := .authored uuid
  source := .goal goal
  target := .memory m
  relation := relId
  owner := goal_owner goal
  authorship := .PerspectiveGoalLink

/-- The motivation edge validates against its descriptor. -/
def motivationValid (goal : Goal) (m : Memory) (relId : RelationId) (uuid : EdgeUuid) :
    EdgeValidWith (motivationDescriptor relId) (motivationEdge goal m relId uuid) where
  relationMatches := rfl
  idAuthorship := by
    constructor
    · intro h
      rcases h with ⟨_, hh⟩
      cases hh
    · intro h
      cases h
  goalCausal := fun _ _ => rfl
  sourceOwned := rfl
  endpointBinding := ⟨trivial, trivial⟩
  ownerPolicy := trivial
  mask := ⟨⟨goal, rfl⟩, ⟨m, rfl⟩⟩
  supersessionEndpointShape := by intro h; cases h

/-- The singleton registry containing the motivation relation descriptor. -/
def motivationRegistry (relId : RelationId) : RelationRegistry where
  descriptors := fun d => d = motivationDescriptor relId
  relationIdUnique := by
    intro d₁ d₂ h₁ h₂ _
    rw [h₁, h₂]

/-- Hence the edge is Causal, hence `m` is motivated by `goal`. -/
theorem motivation_holds (goal : Goal) (m : Memory) (relId : RelationId) (uuid : EdgeUuid) :
    motivatedByGoal (motivationRegistry relId) m goal :=
  ⟨motivationEdge goal m relId uuid,
    ⟨motivationDescriptor relId, rfl,
      motivationValid goal m relId uuid, rfl⟩, rfl, rfl⟩

/-- A genuine single-emission firing: the Mail-Fact arrives, the agent emits
    one Fact `g` owned in a group it may write, created later than the trigger,
    and motivated by the goal. The loop closes — `fires trig.memory g` holds. -/
def oneShotFiring
    (actor : User) (goal : Goal) (config : WakeConfig) (trig : Fact)
    (hcfg : goal_wake goal = some config)
    (harm : goalArmed goal)
    (hactive : goal_state goal = GoalState.Active)
    (hmem : goal_owner goal actor ≠ none)
    (hread : may_read actor (memory_owner trig.memory) .fact)
    (o : Owner) (gid : MemoryId) (schema : SchemaRef) (t : Instant)
    (hw : may_write actor o .fact)
    (hlate : memory_created_at trig.memory < t)
    (relId : RelationId) (uuid : EdgeUuid) : Firing where
  registry := motivationRegistry relId
  actor := actor
  goal := goal
  config := config
  trigger := trig
  emitted := [⟨gid, .Fact, o, schema, none, t⟩]
  injected := []
  invoked := []
  wake_config := hcfg
  armed := harm
  goal_active := hactive
  actor_member := hmem
  trigger_read := hread
  each_injected_read := by intro m hm; simp at hm
  each_fact := by intro m hm; simp at hm; subst hm; rfl
  each_later := by intro m hm; simp at hm; subst hm; exact hlate
  each_authzd := by intro m hm; simp at hm; subst hm; exact hw
  each_motivated := by
    intro m hm; simp at hm; subst hm
    exact motivation_holds goal _ relId uuid
  each_action_allowed := by intro a ha; simp at ha

/-- The emitted Fact of a `oneShotFiring` is genuinely caused by its trigger:
    the causal relation `fires` is non-vacuously inhabited. -/
theorem oneShot_fires
    (actor : User) (goal : Goal) (config : WakeConfig) (trig : Fact)
    (hcfg : goal_wake goal = some config)
    (harm : goalArmed goal)
    (hactive : goal_state goal = GoalState.Active)
    (hmem : goal_owner goal actor ≠ none)
    (hread : may_read actor (memory_owner trig.memory) .fact)
    (o : Owner) (gid : MemoryId) (schema : SchemaRef) (t : Instant)
    (hw : may_write actor o .fact)
    (hlate : memory_created_at trig.memory < t)
    (relId : RelationId) (uuid : EdgeUuid) :
    fires trig.memory ⟨gid, .Fact, o, schema, none, t⟩ :=
  ⟨oneShotFiring actor goal config trig hcfg harm hactive hmem hread o gid schema t hw hlate relId uuid,
    by rfl, by simp [oneShotFiring]⟩

-- ============================================================
-- TARGET 3 — the Active-gate: wakes fire only on Active goals
-- ============================================================

/-- The Active-gate as a projection: a firing's goal is necessarily Active. -/
theorem firing_requires_active (fr : Firing) : goal_state fr.goal = GoalState.Active :=
  fr.goal_active

/-- A terminally-closed goal can NEVER be the subject of a firing — the
    Active-gate forbids it. Once a goal is closed, its wake loop is dead. -/
theorem terminal_cannot_fire (g : Goal) (h : (goal_state g).terminal = true) :
    ¬ ∃ fr : Firing, fr.goal = g := by
  rintro ⟨fr, hfg⟩
  have ha := fr.goal_active
  rw [hfg] at ha
  rw [ha] at h
  exact absurd h (by decide)

-- ============================================================
-- TARGET 2 — self-termination: the close tool bounds the loop
-- ============================================================

/-- The agent closes its own goal: the terminal successor Goal (`Achieved`) that
    supersedes the active one and carries the close-Fact its closing action
    emitted (P3 — a world-touching close emits a Fact). Authoring it is a
    `.goal`-write: "the tool of closing the goal". -/
def closeGoal (goal : Goal) (closeFact : Memory) (hk : memory_kind closeFact = .Fact)
    (newId : GoalId) : Goal where
  id := newId
  owner := goal_owner goal
  schema := goal_schema goal
  title := goal_title goal
  text := goal_text goal
  state := .Achieved
  supersedes := some (goal_id goal)
  authorship := .SystemTool
  close_fact := some closeFact
  wake := none
  terminal_close_fact := fun _ => ⟨closeFact, rfl, hk⟩

/-- The close successor is terminal. -/
theorem closeGoal_terminal (goal : Goal) (closeFact : Memory)
    (hk : memory_kind closeFact = .Fact) (newId : GoalId) :
    (goal_state (closeGoal goal closeFact hk newId)).terminal = true := rfl

/-- The close successor supersedes the original goal (GO-5: a new row). -/
theorem closeGoal_supersedes (goal : Goal) (closeFact : Memory)
    (hk : memory_kind closeFact = .Fact) (newId : GoalId) :
    goal_supersedes (closeGoal goal closeFact hk newId) = some (goal_id goal) := rfl

/-- The close stays in the same owner (GO-1). -/
theorem closeGoal_same_owner (goal : Goal) (closeFact : Memory)
    (hk : memory_kind closeFact = .Fact) (newId : GoalId) :
    goal_owner (closeGoal goal closeFact hk newId) = goal_owner goal := rfl

/-- Closing an ACTIVE goal is an ADMITTED lifecycle transition (Active →
    Achieved): the off-switch is legitimate, not a forced state. -/
theorem closeGoal_admitted (goal : Goal) (closeFact : Memory)
    (hk : memory_kind closeFact = .Fact) (newId : GoalId)
    (hactive : goal_state goal = GoalState.Active) :
    goalTransitionAdmitted (goal_state goal) (goal_state (closeGoal goal closeFact hk newId)) := by
  rw [hactive]; exact trivial

/-- TARGET 2 (headline) — the loop is theoretically BOUNDED. Once the agent uses
    its close tool to author the terminal successor, that goal is `Achieved`, so
    by the Active-gate (`terminal_cannot_fire`) NO firing can target it: the wake
    loop halts. The self-organizing organism holds its own off-switch. -/
theorem closeGoal_halts_wake (goal : Goal) (closeFact : Memory)
    (hk : memory_kind closeFact = .Fact) (newId : GoalId) :
    ¬ ∃ fr : Firing, fr.goal = closeGoal goal closeFact hk newId :=
  terminal_cannot_fire _ (closeGoal_terminal goal closeFact hk newId)

-- ============================================================
-- TARGET 1 — autonomy: while equipped and Active, the loop is unbounded
-- ============================================================

/-- TARGET 1 (sufficiency) — given proper config (armed, ACTIVE, member,
    readable) and Fact-write authority, the actor CAN emit a Fact: a firing
    exists whose emission is genuinely caused by the trigger. Concrete tool
    invocation remains separately bounded by `wake_invoked_actions_allowed`. -/
theorem agent_can_act
    (actor : User) (goal : Goal) (config : WakeConfig) (trig : Fact)
    (hcfg : goal_wake goal = some config)
    (harm : goalArmed goal) (hactive : goal_state goal = GoalState.Active)
    (hmem : goal_owner goal actor ≠ none)
    (hread : may_read actor (memory_owner trig.memory) .fact)
    (o : Owner) (hw : may_write actor o .fact)
    (gid : MemoryId) (schema : SchemaRef) (relId : RelationId) (uuid : EdgeUuid) :
    ∃ g : Memory, fires trig.memory g :=
  ⟨_, oneShot_fires actor goal config trig hcfg harm hactive hmem hread o gid schema
        (memory_created_at trig.memory + 1) hw (Nat.lt_succ_self _) relId uuid⟩

/-- TARGET 1 (the IFF) — proper config fixed, the actor can emit a Fact IFF it
    has Fact-write authority somewhere. Necessity is `powerless_actor_noops`;
    sufficiency is `oneShotFiring`. This is not the external tool allow-list:
    invoked Actions are bounded separately by `WakeConfig.toolset`. -/
theorem act_iff_fact_write_authority
    (actor : User) (goal : Goal) (config : WakeConfig) (trig : Fact)
    (hcfg : goal_wake goal = some config)
    (harm : goalArmed goal) (hactive : goal_state goal = GoalState.Active)
    (hmem : goal_owner goal actor ≠ none)
    (hread : may_read actor (memory_owner trig.memory) .fact)
    (gid : MemoryId) (schema : SchemaRef) (relId : RelationId) (uuid : EdgeUuid) :
    (∃ o : Owner, may_write actor o .fact)
      ↔ (∃ fr : Firing, fr.actor = actor ∧ fr.goal = goal ∧ fr.emitted ≠ []) := by
  constructor
  · rintro ⟨o, hw⟩
    exact ⟨oneShotFiring actor goal config trig hcfg harm hactive hmem hread o gid schema
            (memory_created_at trig.memory + 1) hw (Nat.lt_succ_self _) relId uuid,
           rfl, rfl, by simp [oneShotFiring]⟩
  · rintro ⟨fr, hact, _, hne⟩
    cases hl : fr.emitted with
    | nil => rw [hl] at hne; exact absurd rfl hne
    | cons m ms =>
      have hm : m ∈ fr.emitted := by rw [hl]; exact List.mem_cons_self m ms
      have hwm := fr.each_authzd m hm
      rw [hact] at hwm
      exact ⟨memory_owner m, hwm⟩

/-- The forward run a configured organism produces from a seed and an id supply.
    Step 0 is the seed; each later Fact lands one instant after the previous, so
    times strictly increase and the run never stalls. -/
def autonomousRun (seed : Fact) (o : Owner) (schema : SchemaRef) (ids : Nat → MemoryId) :
    Nat → Memory
  | 0 => seed.memory
  | (n+1) => ⟨ids n, .Fact, o, schema, none, memory_created_at seed.memory + (n+1)⟩

theorem autonomousRun_fact (seed : Fact) (o : Owner) (schema : SchemaRef) (ids : Nat → MemoryId) :
    ∀ n, memory_kind (autonomousRun seed o schema ids n) = .Fact
  | 0 => fact_memory_kind seed
  | (_+1) => rfl

theorem autonomousRun_time (seed : Fact) (o : Owner) (schema : SchemaRef) (ids : Nat → MemoryId) :
    ∀ n, memory_created_at (autonomousRun seed o schema ids n)
        = memory_created_at seed.memory + n
  | 0 => by simp [autonomousRun]
  | (_+1) => rfl

/-- TARGET 1 (headline) — THE ORGANISM IS AUTONOMOUS. Properly configured
    (armed, ACTIVE, authorized member, readable) and equipped with Fact-write
    authority plus an id supply, the actor produces an ENDLESS forward run of
    Facts, each CAUSED by the previous, with NO external input after the seed.
    External tool invocation is bounded separately by `WakeConfig.toolset`; this
    theorem proves the Fact-emission loop. The loop runs forever — the dual of
    `organism_grounded`: backward causation terminates, forward causation need
    not. It runs "as long as wake entries are produced", i.e. as long as the
    goal stays Active. -/
theorem organism_autonomous
    (actor : User) (goal : Goal) (config : WakeConfig) (seed : Fact)
    (hcfg : goal_wake goal = some config)
    (harm : goalArmed goal) (hactive : goal_state goal = GoalState.Active)
    (hmem : goal_owner goal actor ≠ none)
    (hread : may_read actor (memory_owner seed.memory) .fact)
    (o : Owner) (hw : may_write actor o .fact)
    (schema : SchemaRef) (ids : Nat → MemoryId) (uuids : Nat → EdgeUuid) (relId : RelationId) :
    ∃ run : Nat → Memory, ∀ n : Nat, fires (run n) (run (n+1)) := by
  refine ⟨autonomousRun seed o schema ids, fun n => ?_⟩
  have hlate : memory_created_at (autonomousRun seed o schema ids n)
      < memory_created_at seed.memory + (n+1) := by
    rw [autonomousRun_time seed o schema ids n]; exact Nat.lt_succ_self _
  have hread_n : may_read actor (memory_owner (autonomousRun seed o schema ids n)) .fact := by
    cases n with
    | zero => exact hread
    | succ _ => exact may_write_implies_read actor o .fact hw
  exact oneShot_fires actor goal config
    ⟨autonomousRun seed o schema ids n, autonomousRun_fact seed o schema ids n⟩
    hcfg harm hactive hmem hread_n o (ids n) schema (memory_created_at seed.memory + (n+1))
    hw hlate relId (uuids n)

-- ============================================================
-- THE openness guarantee — only pre-existing kernel axioms
-- ============================================================

#print axioms organism_grounded
#print axioms wake_cannot_escalate
#print axioms powerless_actor_noops
#print axioms wake_action_has_goal_context
#print axioms wake_invoked_actions_allowed
#print axioms wake_motivation_is_perspectival
#print axioms oneShot_fires
#print axioms terminal_cannot_fire
#print axioms closeGoal_halts_wake
#print axioms act_iff_fact_write_authority
#print axioms organism_autonomous

end Causa.Wake
