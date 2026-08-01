# Kernel Coverage Matrix — docs → carriers

This file maps each documented invariant to its **kernel carrier**: the Lean
declaration in `Causa` that carries it, or an explicit statement that the kernel
does not cover it and why.

**Nothing machine-checks the names in this table.** `lake build` checks that the
kernel compiles and `scripts/check-lean-axioms.py` pins its axiom surface at
zero, but neither reads this file, so a cell naming a declaration that no longer
exists reads wrong without failing anything. A claim to the contrary stood here
until 2026-08-01 and was itself an instance of the decay the note below
describes. Cells naming a RETIRED declaration say so in words; cells naming a
live one are only as good as the last person who checked.

Carrier values: a named `theorem`/`def`/`structure` is the claim's carrier;
`structural` means the offending capability has no representation to begin with;
`partial` means the kernel carries a consequence but not the whole claim;
`excluded` means the kernel deliberately does not model it, with the reason;
`ASSERTED` marks a rule the kernel states as table validity or a structure field
rather than proving — those are named as such on purpose, because a definition
dressed as a theorem is the same decoy in a different costume.

**There is deliberately no "runtime enforcement" column.** One existed until
2026-07-31 and was removed: it was prose asserting facts about Rust and SQL with
nothing checking it, so it decayed silently and then misled the people who
trusted it. Three cells were actively wrong at once — `CI-15/16` declared the
whole blob lane `unchecked` while four live request-path gates existed, `U-4`
named one verb as "the sole Fact-creating path" when there were two, and the
flavor matrix described an upsert as `ON CONFLICT DO NOTHING` when the code says
`DO UPDATE … RETURNING`, an error that would have broken every replay had anyone
"corrected" the code to match. None of them failed anything.

Runtime enforcement is the tests' job, because a test either runs or it does
not. When you want to know whether something is enforced at runtime, read the
test — and check it fails when the behaviour is removed, which is the only thing
that distinguishes a carrier from a decoy.

## universe.md (U)

| ID | Invariant | Carrier |
|---|---|---|
| U-1 | Layering strict & irreversible; no lower-layer memory from higher layer | theorem `fact_memory_kind` + THEOREM `operator_memory_output_not_fact` (from the phase output contract) + THEOREM `edge_layer_rule` (E3); header comments Memory.lean |
| U-2 | Causal claims perspective-relative; no semantic/causal Fact→Fact; Perspective is locus of causal claims | REBASED on the v0.0.8 model, in two parts. (a) STRUCTURAL: the closed `EdgeKind` has no Causal/Interpretive variant, so there is no row a causal claim could occupy — THEOREM `principle_epistemic_edge_kinds_are_exactly_two` exhausts the vocabulary. (b) DEFINITIONAL + THEOREM: `interpretationOf` DEFINES an interpretation as a Perspective whose payload references its subjects, and `fact_source_reaches_only_facts` (from E3) proves a Fact source can never reach an Abstraction or Perspective to interpret it. The definitional half is stated as such — with the class matrix retired there is no matrix cell left to prove it from, and pretending otherwise would be a decoy |
| U-3 | Dreaming = flavor-declared consolidation; no Dream entity/relation/pipeline | structural absence; Operators.lean header comment |
| U-4 | Reality enters as typed Facts | `Fact` subtype (`Memory` with kind `.Fact`) + `operator_memory_output_not_fact`; `Flavor.OptionalFactReceipt` witnesses optional event/receipt metadata without certifying external truth |

## 01 — Source / Fact ingest (ES)

| ID | Invariant | Carrier |
|---|---|---|
| ES-1 | Group org → no kernel face | excluded: org has no kernel face and the Owner=OwnerRef collapse is absent from Core storage and identity — `OwnerRef` / resolved `Owner := Group` carry no org predicate; tenancy is a flavor/app concern. Decisions `2026-06-11-org-out-of-kernel.md`, owner-ontology realign 2026-06-28 (was THEOREM `owner_org_denormalized` when Owner carried org) |
| ES-2 | Visibility rule (group membership) | def `visible` (`o r ≠ none`, i.e. holds any `Role`) + theorem `visible_personal` |
| ES-3 | org never enters access or identity | structural: org absent from `OwnerRef`, `OwnerState`, and resolved `Owner := Group` — decisions `2026-06-11-org-out-of-kernel.md`, owner realign 2026-06-28 |
| ES-4 | source-ingest dedup key deterministic over source/owner/payload | excluded: source/flavor ingest metadata after D1; no core `FactReceiptId` entity |
| ES-5 | Batch id unique within (source, owner) | excluded: per-scope source-id validation has no kernel-observable face; F→A gate carries owner dimension separately under CN-8; wake context dimension deferred after D4 |
| ES-6 | Facts are typed observations, not operator derivations | `Fact` subtype + `operator_memory_output_not_fact`; `Flavor.OptionalFactReceipt` models arbitrary observed-source receipts as optional metadata |
| ES-7 | 1:1 source-ingest receipt→Fact materialization | partial: `Flavor.OptionalFactReceipt` proves receipt payloads attach only to Fact rows; exact source materialization/idempotency remains source/flavor ingest implementation |
| ES-8 | Source must not abstract/interpret/cross-join/relevance-filter/persist | excluded: source/flavor ingest contract; kernel carries the consequence via `operator_memory_output_not_fact` + no downward operator writes |
| ES-9 | Compliance metadata: every source declares 4 fields; Facts inherit | excluded: engine registration totality (CO-39..45 rows); source identity is engine-side — kernel `SourceId` axiom retired 2026-06-28 (unused once Compliance dropped `DeleteSourceScope`) |
| ES-10 | Idempotency keys content-derived/opaque, never natural-person identifiers | excluded: source/flavor ingest concern with no kernel carrier (suppression docstring retired 2026-06-28 with the `SuppressionKey` axioms) |
| ES-11 | Facts' typing frozen at insert; engine does not migrate Facts across schema versions | `Immutable Memory`-stance via `AppendOnly Memory` + accessor totality (`memory_schema` fixed per row); migration mechanics excluded (SR-50..55 exclusion block) |
| ES-12 | No legacy org-principal variant; org-wide = `<org>-everyone` group | structural: stable ownership uses `OwnerRef.world` / `OwnerRef.personal u` / `OwnerRef.group id`; no Org variant. Resolved owner is a Group (`User → Option Role`). Realign 2026-06-28; no-op under 2026-07-06 User token change |
| ES-13 | Per-memory ACL / `AccessGrant` layer is absent | structural absence + Owner.lean header comment |
| ES-14 | Push vs pull is source-side implementation detail | excluded: engine |
| ES-15 | Bootstrap/founding-goal is flavor onboarding | excluded: app-layer |
| AUTH-SHARE | One owner per entity; sharing IS group membership (no share set above the owner) | structural: entity rows should carry one stable `OwnerRef`; `OwnerState.resolve` yields the single resolved Group for access. Read-only share = viewer-role membership, publish = `.world`. No `Scope`/`reaches` multi-owner layer — realign 2026-06-28 |
| AUTH-READ | Read = role read-ceiling over the kind, in the server-resolved owning group | defs `may_read` over resolved `Owner` and `may_read_in` over stable `OwnerRef` + `OwnerState.resolve`; role model `Role` (`Role.mayRead`) |
| AUTH-WRITE | Write = role write-ceiling in the server-resolved single owning group | defs `may_write` over resolved `Owner` and `may_write_in` over stable `OwnerRef` + `OwnerState.resolve`; `Role.mayWrite` |
| AUTH-MANAGE | Meta-management = managing a group's membership/role map | defs `may_manage` / `may_manage_in`; personal groups forbidden; `Role.manage` flag + `Role.admin` preset |
| AUTH-MANAGE-P | Personal groups forbid meta-management | THEOREMs `personal_forbids_manage`, `owner_state_personal_forbids_manage`; structural — `may_manage` requires `¬ Owner.isPersonal`, and `personal.manage = false` |
| AUTH-MANAGE-W | World group forbids meta-management | THEOREMs `world_forbids_manage`, `owner_state_world_forbids_manage`; structural — every World member is `viewer` (`manage = false`), independent of the personal-group rule |
| AUTH-1 | Write ⊆ read (whoever may write may read) | THEOREMs `may_write_implies_read`, `may_write_in_implies_read_in`, from structural field `Role.write_le_read` after owner-state resolution |
| AUTH-2 | World is read-only (never a write target) | THEOREMs `world_read_only`, `owner_state_world_read_only`, from `world := fun _ => some Role.viewer` and `OwnerState.world_resolves` |
| AUTH-3 | World is universally readable | THEOREMs `world_universally_readable`, `owner_state_world_universally_readable`, from World's `viewer` read ceiling covering every kind |
| AUTH-4 | Per-memory ACL / owner-space grants absent — access is role-graded group membership | structural: no `AccessGrant`/`MemoryAction`; `Group := User → Option Role`; realign 2026-06-28; no-op under 2026-07-06 User token change |
| AUTH-EDGE | Index write admission = source write + target read, one uniform rule | def `edge_write_admitted` (three conjuncts, no policy lookup); THEOREMs `edge_write_admitted_valid`, `edge_write_admitted_source_write`, `edge_write_admitted_target_read`, `cross_owner_target_admitted`, `edge_write_admitted_owns_row`. `targetAccessSatisfied` / `RelationTargetAccessPolicy` retired with the descriptor |
| AUTH-EDGE-READ | Index read is source-local; target projection is separately gated/redacted | defs `edge_read_admitted`, `edge_target_readable`, `edge_target_available`, `edge_target_redacted`; THEOREMs `edge_read_admitted_source_owned`, `target_unreadable_redacts_edge_target`, `target_abandoned_redacts_edge_target` |
| NEST-1 | Group nesting needs no new primitive — a nested group resolves to an ordinary `Owner` | def `Role.meet`/`Role.join` (role lattice) + `Group.mount`/`Group.union`; the kernel sees only the resolved `Owner`, nesting is host composition (Level 2, 2026-06-28) |
| NEST-2 | Capped mounting cannot escalate write authority | THEOREM `mount_cannot_escalate` — if the cap may not write kind `k`, no member of the mounted group gains write `k` (meet caps the write ceiling) |
| NEST-3 | Union grants at least each side's access | THEOREM `union_grants_each` — read via either group ⇒ read via the union (join never lowers a member's capability); write case analogous |

## 02 — Memory (ME)

| ID | Invariant | Carrier |
|---|---|---|
| ME-1 | Fact is Memory with kind `.Fact` | subtype `Fact := { m : Memory // memory_kind m = .Fact }` + theorem `fact_memory_kind`; runtime SQL currently encodes the Fact branch as `memories.kind IS NULL` while preserving the kernel distinction from derived kinds |
| ME-2 | Fact owner is the memory row owner | structural: `Fact.memory` projects to `Memory.owner`; source/event owner inheritance moved out of core by D1 |
| ME-3 | Optional free text is a Memory field for F/A/P; no kind-based text axiom | structure field `Memory.text : Option Text` + accessor `memory_text` |
| ME-4 | Facts never supersede / never superseded | THEOREM `facts_never_supersede` (Memory.lean) — source half from the ROW field `Memory.fact_never_supersedes` (mirroring the row-local `memories_variant_chk` Fact branch), target half from `MemorySupersessionValid.sameKind`. Supersession is a lineage pointer, so no edge is involved |
| ME-5a | Supersession same kind | table validity `MemorySupersessionValid.sameKind` + projection THEOREM `memory_supersession_same_kind`. ASSERTED, not derived: with the class matrix gone there is nothing to prove it from, and the runtime asserts it too (`validate_supersedes_in_owner` binds `m.kind = $4`) |
| ME-5b | Supersession same owner | table validity `MemorySupersessionValid.sameOwner` + projection THEOREM `memory_supersession_same_owner`. ASSERTED, not derived — the descriptor `ownerPolicy` cell it used to be derived from is retired; the runtime asserts the same thing (`validate_supersedes_in_owner` scopes the prior row to the writer's owner) |
| ME-6 | Personality is not a materialized Memory author/owner slot | structural absence: no `PersonalityInstance`, no `personality_owner`, no `memory_authoring_personality`; D4 comment Memory.lean |
| ME-7 | Facts below Perspectives; no personality read-scope matrix | theorem `principle_1_facts_below_perspective`; structural absence of `read_scope`/`personality_may_read`; wake trigger/context reads use `Wake.Firing.trigger_read` and `each_injected_read` over actual memory owners |
| ME-8 | Materialized personality matrix removed | structural absence of `read_scope` and matrix-version state; wake context/read semantics are role-graded Owner checks in `Wake.Firing`, not a personality matrix |
| ME-9 | Index rows source-owned (E2); supersession intra-Owner | row-validity predicate `EdgeSourceOwned` (field of `EdgeValid`) + projection THEOREM `edge_source_owned`; also THEOREM `declared_edges_valid`, which makes E2 hold BY CONSTRUCTION for derived rows. Supersession intra-Owner is ME-5b, off the row, not an edge policy |
| ME-10 | ℓ(source) ≥ ℓ(target) for valid memory rows (E3) | def `EdgeLayeringValid` (field of `EdgeValid`, stated over `EndpointKind.layer` exactly as the SQL CHECK is) + THEOREM `edge_layer_rule`; FactEntity heads are Fact-like through `NodeRef.memoryKind?` / `EndpointKind.layer`, Goal endpoints carry no layer |
| ME-11 | Class-legality matrix (9 cells) | RETIRED with the class vocabulary. The nine cells said exactly ℓ(source) ≥ ℓ(target) once the kinds closed at two that neither widen nor narrow the matrix (doc 02 §The Directionality Rule: "`origin` and `reference` alike"), so ME-10's `EdgeLayeringValid` carries the whole of it. `legalClasses`, `masksTightenOnly`, `edge_class_legal*` deleted |
| ME-12 | Supersession same endpoint shape (incl. Goal→Goal) | STRUCTURAL, by typing: `Memory.supersedes : Option MemoryId` can only name a memory and `Goal.supersedes : Option GoalId` only a goal, so a mixed Memory/Goal supersession is not expressible. `EdgeSupersessionEndpointShapeValid` and `supersession_same_endpoint_shape` deleted with the Supersession class |
| ME-13 | Index rows immutable v1 | `Edge` structure + instances `Immutable Edge`, `AppendOnly Edge` |
| ME-14 | Descriptor masks tighten, never relax | RETIRED: there are no descriptors and no masks (doc 16 §What This Removes). What replaces the tightening obligation is `NodeDeclarationValid` — legality is checked on the DECLARATION, and THEOREM `declared_edges_valid` shows a legal declaration derives rows satisfying E2/E3, so nothing can relax the layer rule by construction |
| ME-15 | Causal chain is a query, not an entity; materialized = cache only | structural absence + Edges.lean header (the chain is now: reference backbone + interpretation Perspectives + origin closure) |
| ME-16 | Memory id is identity | structure field `Memory.id` + table/store invariant `MemoryIdUnique` |
| ME-17 | Personality is emergent from Perspective/wake context, not a stored instance | structural absence in Memory.lean/Principles.lean; no Personality module; `selfPerspectives` queries existing Perspective rows by owner |
| ME-18 | Cross-context supersession policy | excluded: wake/Perspective context semantics deferred after D4; no personality instance axis in kernel |
| ME-19 | Relation registry: unregistered relations invalid | RETIRED with the relation layer. There is no relation to register: the kind follows the operation, so `RelationRegistry`, `RelationDescriptor`, `RelationId` and the registry parameter threaded through every edge predicate are deleted. E4 (`derived_edge_kind_follows_operation`) is what now stands between a row and existence |
| ME-20 | Core relations table (derived-from/supersedes/inspires/authored) | RETIRED: every entry moved to the node that owns the statement — `derived-from` → `origin` from the write's declaration; `supersedes` → `Memory.supersedes` / `Goal.supersedes`; `inspires` → `Goal.assignment`; `authored` → `Memory.authoring_perspective`; `depends-on` → `Goal.dependencies`; `motivated-by` → `Goal.evidence`. The kind vocabulary is `EdgeKind`, closed at two |
| ME-K1 | Text-bearing Memory rows can be model-independent knowledge artifacts | `KnowledgeContent := Text`; `InterpreterKind` / `InterpreterClass`; `KnowledgeArtifact` requires `memory_text carrier = some text` plus class-level recoverability; THEOREMs `knowledge_artifact_has_text`, `knowledge_artifact_model_independent`, `knowledge_artifact_recoverable_by_its_kind` |
| ME-K2 | Long-term knowledge artifact = admitted text-bearing Memory row, not one model cache | def `KnowledgeArtifactIn memories artifact`; THEOREM `long_term_knowledge_artifact_has_text_memory`; no `Truth`/`Knows`/specific LLM or human instance in core |

## 16 — Edges (E) — the kernel invariants of doc 16

The edge obligations in full. E7 is the master invariant; E4–E6 are its
preconditions, and E2/E3 fall out of it for any table that is actually
rebuilt.

`Causa/Edges.lean` ends with an axiom block over the fourteen HEADLINE E1–E7
theorems — not over every declaration in the file — and it is the one part of
this table that checks itself, but only because the block is
`#guard_msgs`-pinned. A bare `#print axioms` would NOT check anything:
it emits an `info` message, so a proof that started depending on an axiom would
print a different line and the build would still go green. With each expected
message pinned in a docstring, a changed axiom surface is a build ERROR, and so
is a theorem that stops existing. Confirmed by negative control — corrupting one
expectation fails the build with "Docstring on `#guard_msgs` does not match".

All fourteen of those are axiom-free OUTRIGHT: not merely free of `Causa` axioms, but of
`propext` and `Quot.sound` as well. That is stronger than the kernel-wide
policy, which is what `scripts/check-lean-axioms.py` pins — that script counts
DECLARED `Causa` axioms (zero, everywhere), while this block pins what these
particular theorems CONSUME. Elsewhere in the kernel the weaker property is the
honest one and is stated as such: `Flavor.published_readable`,
`Wake.organism_autonomous` and others do depend on `propext`/`Quot.sound`.

| ID | Invariant | Carrier |
|---|---|---|
| E1 | Existence — both endpoints exist | def `NodeRefInTables` + def `EdgeEndpointsExist`; admitted-graph field `MemoryGraphValid.edgeEndpointsPresent` + projection THEOREM `memory_graph_edge_endpoints_exist`. TABLE VALIDITY (ASSERTED), not a theorem: whether a row's endpoints are present is a fact about the store, which is what the runtime existence trigger checks. It does real work downstream — THEOREM `nodeRef_addr_determines_row` needs it to turn an address back into a row |
| E2 | Ownership — `edge.owner = source.owner`; the TARGET is unconstrained | def `EdgeSourceOwned` (field of `EdgeValid`) + THEOREM `edge_source_owned`; THEOREM `declared_edges_valid` derives it by construction; THEOREM `cross_owner_target_admitted` (EdgeAuthorization) states the other half — a cross-owner target needs only target READ authority, with no policy cell to consult |
| E3 | Layering — ℓ(source) ≥ ℓ(target) for memory endpoints, Goal endpoints outside | def `endpointsLayered` / `EdgeLayeringValid` over `EndpointKind.layer` (the SQL CHECK's own shape) + THEOREMs `edge_layer_rule`, `fact_source_reaches_only_facts`; derived by construction from a legal declaration (`declared_edges_valid`) |
| E4 | Kind follows operation; no free-standing edge write | def `NodeDeclaration` + def `NodeDeclaration.edges` + def `deriveEdges`; THEOREMs `derived_edge_kind_follows_operation`, `origin_row_needs_a_derivation_declaration`, `reference_row_needs_a_declared_reference_field`. Raw `Edge` values stay constructible (the D14/D16 discipline); E4 governs ADMISSION, i.e. membership in a rebuildable table |
| E4z | A write with ZERO origins is legal | THEOREM `declaration_without_origins_writes_no_origin_rows` + constructive witness `interpretationDeclaration` / `interpretation_declaration_writes_only_references`; on the operator side THEOREM `invocation_without_inputs_is_complete` — the manifest proves a derivation, and a write with none has nothing to prove, so it is skipped rather than failed |
| E5 | Structural idempotency — the primary key IS the row | STRUCTURAL: no `EdgeId`/`ContentHash`/`EdgeUuid` type exists any more (Identity.lean). def `edgeKey` is `edges_pkey` column for column. THEOREMs `edge_key_determines_row` (two valid rows with the same endpoints and kind are the SAME VALUE — false under edge ids, which is why v0.0.7 needed a content hash), `nodeRef_addr_determines_row`, `edge_table_key_unique` (the PK itself, PROVED from E1 + row-id uniqueness + E2 rather than assumed as a table rule), `assert_present_row_changes_nothing`, `replay_asserts_nothing_new` |
| E6 | No content — no payload, citation or status | STRUCTURAL ABSENCE on `Edge` (four fields, three of which are the key), sharpened by `edge_key_determines_row`: there is no field two rows with one key could differ in. `OptionalEdgeSidecar` deleted from Causa.Flavor |
| E7 | Rebuildability — the edge set is a function of node content | def `deriveEdges` (THE function) + def `EdgeTableRebuildable`; THEOREMs `derived_table_rebuildable`, `rebuild_deterministic`, `rebuilt_table_valid` (a store with legal declarations rebuilds into a VALID index), `principle_9_index_is_a_function_of_node_content`. Goal side: def `GoalDeclarationValid` + THEOREMs `goal_declared_rows_are_references`, `goal_declared_row_count` |
| E-KIND | Two kinds, closed, not flavor-extensible | inductive `EdgeKind` (`origin`, `reference`); THEOREM `principle_epistemic_edge_kinds_are_exactly_two` exhausts it |
| E-NODE | Interpretation is a node, not a kind | def `interpretationOf` (a Perspective whose payload references its subjects) + THEOREMs `interpretation_is_never_a_fact`, `interpretation_rows_are_references`. DEFINITIONAL where the retired matrix was structural — see U-2 |
| E-TIME | `created_at` on the row | EXCLUDED, deliberately: it is the one runtime edge column that is NOT a function of node content, so modeling it would make E7 false as stated. No kernel obligation reads it; the kernel's time axis is `Memory.created_at` |

## 04 — Consolidation (CN)

| ID | Invariant | Carrier |
|---|---|---|
| CN-1 | F→A input/output shape | `OperatorPhase.inputKind`/`outputMemoryKind` + `InvocationShapeValid`; THEOREMs `operator_inputs_match_phase`, `invocation_output_memory_kind_valid`. The edge-shape carrier is retired: there is no authorship column to match a phase against, and the origins a write declares ARE its inputs |
| CN-2 | A→P input/output shape | same carrier: `InvocationShapeValid` + `operator_inputs_match_phase`; the output kind is `.Perspective` by `OperatorPhase.outputMemoryKind` |
| CN-3 | A→Goal evidence shape | `OperatorPhase.outputGoalAllowed` + `InvocationProvenanceComplete.goalInputs` (a Goal output declares its inputs as `reference` rows, because a Goal rests on them rather than deriving from them) + `OperatorPhase.inputEdgeKind` |
| CN-4 | frame — `P × A_cross → P` | RETIRED as an edge shape. A frame is a Perspective whose payload references the cross-domain Abstraction (doc 02 §The Layering Principle), i.e. an ordinary `reference` declaration; `interpretationDeclaration` is its constructive witness and `PerspectiveLink` has no successor |
| CN-5 | No downward writes | THEOREM `operator_memory_output_not_fact` (from the phase output contract alone — F→A/A→A give Abstraction, A→P gives Perspective, A→Goal gives no memory row) + THEOREM `operator_origin_row_not_upward` (E3 on the ledger's own rows) + ME-1 Fact subtype |
| CN-6 | Derived memories have valid provenance | table-scoped `MemoryGraphValid.derivedProvenance` (every admitted non-Fact row declares at least one admitted memory it rests on — origins if it derived, references if it interprets). THEOREM `abstraction_has_provenance` keeps its Fact ∨ Abstraction target, now pinned by E3 rather than by the matrix. `perspective_has_provenance` is WEAKENED: it no longer concludes the target is an Abstraction, because P→F and P→P are ordinary legal rows and an interpretation Perspective references its subjects directly |
| CN-7 | Cross-domain join is typed Abstraction | comment (shape carried by CN-6 + U-2 matrix) |
| CN-8 | F→A batch-gate exclusivity per (owner, batch, input contract, operator, output schema) | admitted-graph validity field `MemoryGraphValid.ftoaBatchExclusive` over structure `FtoaBatchExclusive memories` + projection THEOREMS `memory_graph_ftoa_batch_exclusive`, `ftoa_batch_exclusive`; operator/batch/contract metadata are `Memory` fields (`memory_operator`, `memory_source_batch`, `memory_input_contract`); wake context dimension deferred after D4, no personality dim |
| CN-8b | Operator invocation input completeness | `OperatorInvocation` ledger witness + `InvocationInGraph` / `InvocationShapeValid` / `InvocationEdgeShapeValid` / `InvocationProvenanceComplete`; THEOREMs `invocation_memory_input_provenance_persisted` (memory outputs declare `origin` rows), `invocation_goal_input_evidence_persisted` (goal outputs declare `reference` rows). A write with no derivation declaration carries NO manifest — THEOREM `invocation_without_inputs_is_complete` (E4z) |
| CN-9 | Atomic invocation (all-or-nothing outputs) | excluded: storage-layer transaction contract (same stance as WH event/projection atomicity); Lean only validates an admitted invocation ledger/manifest |
| CN-10 | Retry/changed-prompt = new derivation, never mutation | `AppendOnly Memory` + comment Operators.lean |
| CN-11 | Wake dispatcher loop, cursors, depth bound, runtime tables | excluded: engine runtime |
| CN-12 | Prompt locality (core ships no domain prompts) | excluded: engine/flavor split mechanics; spirit carried by CF-A |

## 06 — Goals (GO)

| ID | Invariant | Carrier |
|---|---|---|
| GO-1 | Supersession same owner | table validity `GoalSupersessionResolved` + `GoalSupersessionValid`; projection THEOREM `goal_supersession_same_owner` |
| GO-2 | Valid lifecycle transition | def `goalTransitionAdmitted` + table validity `GoalSupersessionResolved` + `GoalSupersessionValid`; projection THEOREM `goal_supersession_admitted` |
| GO-3 | Goal DAG acyclic | no legacy parent table and no DAG primitive. Goal↔Goal topology is the `dependency_goal_ids` column (`Goal.dependencies`), from which `reference` index rows are derived; relation-specific acyclicity is engine validation, not a Goal-row invariant |
| GO-4 | Parents same owner | retired with Goal-local parents; the derived rows are source-owned by the declaring Goal (E2) and layer-exempt (Goal endpoints carry no layer), with no descriptor mask left to consult |
| GO-5 | Every transition new row; no in-place mutation | `AppendOnly Goal`; supersession stores prior `GoalId` and current state is a table query |
| GO-6 | Goal is not Memory | structural: distinct Types |
| GO-7 | Self is a query, never an entity/cache | structural absence + `selfGoals` / `selfPerspectives` query defs and projection theorems in Goals.lean; head-aware Perspective projection is `perspectiveHeads`, now in Memory.lean because supersession is a row pointer |
| GO-8 | Active set definition (heads, state=Active) | table-scoped defs `goalIsHead`, `activeGoals`; supersession traversal `GoalSupersessionReachable` + `activeGoalHeadFrom` |
| GO-9 | Goal id is identity | table validity `GoalIdUnique`; projection THEOREM `goal_id_injective` |
| GO-10 | Authorship vocabulary | inductive `GoalAuthorship` |
| GO-11 | GoalWrite protocol (request_id idempotency, conflict detection, stream visibility) | excluded from Goal ontology: request-id/body replay is protocol/write-atom state (doc 14), not a Goal row invariant; item 10 resolved by keeping it out of `Goal`/Self |
| GO-12 | Assignment is the Goal row's `assignment_perspective_id`; instance-scoped active_goals query | `goalAssignedToPerspective` reads `Goal.assignment` (no edge, no relation id, no Self row) + `activeGoalsForSelf` follows `GoalSupersessionReachable` and returns Active heads; projection THEOREMs `goal_assignment_target_perspective`, `active_goal_for_self_active/_head/_has_assignment` |
| GO-13 | Goal-scoped wake policy; planner-first | `Goal.wake : Option WakeConfig` + `Wake.Firing.wake_config` bind firing to the Goal-owned config; `actor_member` is any server-resolved role/grant in the Goal owner, not owner equality or Goal-write; `trigger_read`/`each_injected_read` use actual memory owners; `each_authzd` gates emitted Facts; `each_action_allowed` pins invoked Actions to `WakeConfig.toolset`; dispatcher scheduling remains engine runtime |
| GO-14 | Goal assignment/evidence scope | Goal rows carry Owner; assignment/evidence are Goal ROW COLUMNS. `GoalEvidenceValid` requires every declared `evidence_memory_ids` entry to resolve to an admitted non-Perspective memory and every `SystemOperator` Goal to declare at least one; THEOREMs `system_operator_goal_has_evidence`, `goal_evidence_not_perspective`. The evidence-shape half is table validity (asserted), mirroring `validate_evidence_in_owner` / `validate_operator_goal_evidence` |
| GO-17 | Root Goal creation shape | `GoalRootValid` + THEOREM `goal_root_active`: roots (`supersedes = none`) are Active only |
| GO-18 | Terminal close Fact table validity | `GoalTerminalCloseFactValid` + projection THEOREMS: close Fact is a memory-table Fact with same Owner as terminal Goal |

## 03 — Schema Registry (SR) — in-kernel rows

| ID | Invariant | Carrier |
|---|---|---|
| SR-1 | Registry frozen at startup, no runtime registration | partial: the relation registry is GONE (nothing to freeze — the kind follows the operation); wake `Action` values are allowed only by the Goal's `WakeConfig.toolset` (`wake_invoked_actions_allowed`), while concrete schema/tool/source/prompt registry freeze and flavor linking remain build-time engine mechanics (Composition.lean deleted 2026-06-28, D16) |
| SR-2 | Every memory payload schema-typed | structure field `Memory.schema : SchemaRef` (accessor totality — every row carries an opaque schema tag). Schema registration in the active registry remains engine admission, not yet a kernel rule (D16) |
| SR-8 | Schema ids flavor-qualified | excluded: namespacing is engine id-minting (collision-freedom = "the engine mints distinct ids"), not kernel ontology (D16) |
| SR-11/16 | F/A/P may carry optional free text; sidecars may carry opaque typed payload | structure field `Memory.text : Option Text`; `Flavor.OptionalMemorySidecar` and `OptionalGoalSidecar` are constructive optional wrappers whose payloads are forgotten by kernel invariants; no sidecar-required law. There is NO edge sidecar — E6 |
| SR-13 | Fact identity ≠ payload hash; UUIDv7 | structure field `Memory.id` + table/store invariant `MemoryIdUnique` + ST-22 comment |
| SR-14/44 | Fact has no supersedes | structure field `Memory.fact_never_supersedes` (row-local, mirroring `memories_variant_chk`) + THEOREMs `fact_supersedes_nothing`, `facts_never_supersede` |
| SR-24 | edge kind closed, not flavor-extensible | inductive `EdgeKind` — two variants (`origin`, `reference`), not five classes under an open relation vocabulary. The `relation_class` enum, its namespaced relation ids, and `EdgePayload` are all deleted (doc 16 §Kinds are closed); a feature that seems to need a third kind is missing a node |
| SR-25 | Index rows immutable v1 | `Immutable Edge` |
| SR-30..33 | special_category per schema, author-declared | excluded: GDPR controller/engine concern (`schema_special_category` cut 2026-06-28, D16) — the kernel never reasons over special-category |
| SR-43 | Stateful Fact: each observation a new Fact | `FactEntity` current head is a `Fact` (`factEntityCurrentIsFact`); `FactEntity` is a head aggregate, not replacement/supersession of Fact rows |
| SR-46 | Tombstone is a Fact (deletion is observed state) | comment Identity.lean / excluded detail: stateful-schema mechanics |
| SR-49 | Memory row stores schema id+version+kind | accessors `memory_schema`, `memory_kind` |
| SR-56/57 | Layer/kind change in migration forbidden | excluded: migration mechanics; spirit = kind is fixed per memory (accessor totality, immutability) |

## 08 — Core & Flavors (CF) — in-kernel rows

| ID | Invariant | Carrier |
|---|---|---|
| CF-G | Payload opacity — the domainless boundary | structural ABSENCE: `SchemaRef` (Identity) is an opaque per-row tag with NO accessor at all (no resolution, no payload, no capabilities); `Flavor.OptionalMemorySidecar` / `OptionalGoalSidecar` prove sidecar payload changes do not affect kernel-visible rows. Opacity is also why `NodeDeclaration` carries resolved endpoints rather than payload fields: WHICH payload fields are reference fields is schema-registry knowledge below the boundary |
| CF-* (compliance) | A flavor must comply with the basic rules | THEOREMS in `Causa.Flavor` — a concrete flavor's rows discharge the universal invariants (`fact_is_fact`, `perspective_is_perspective`, `abstraction_grounded`, `flavor_perspective_has_provenance`, `flavor_declared_edges_valid`, `published_readable`, `published_read_only`, `wipeable_when_abandoned`) via only pre-existing theorems; the rules quantify over every row, so compliance is derived, never axiomatized |
| CF-OPEN | Substrate is OPEN — any app integrates as a flavor with zero kernel change and no new Causa axiom | `Causa.Flavor` constructive witness: a flavor's vocabulary is inhabitants of the existing opaque type `SchemaRef`, taken as a PARAMETER; `#print axioms` on the flavor theorems names no Causa axioms — never one named `flavor`. That machine-checked absence IS the openness (D16/D18). NARROWED in v0.0.8: `RelationId` is gone, so a flavor can no longer mint traversable link vocabulary — the deliberate loss recorded in doc 16 §What This Removes. The escape valve is an interpretation node, and it is total |
| CF-43..46 | Goal entity core-owned; flavor owns payload/tools | structural: Goal lives in the kernel; payloads opaque |
| CF-60 | Cross-flavor reads obey Owner/access surface | Owner/read authorization (Causa.Authorization); no materialized personality read-scope after D4 |

## 07 — Storage (ST) — in-kernel rows

| ID | Invariant | Carrier |
|---|---|---|
| ST-1..4 | Fresh ids; immutable identity; supersession = new row | `Memory.id` + `MemoryIdUnique`, `Goal.id` + `GoalIdUnique`, classes `Immutable`/`AppendOnly`; memory supersession stores the prior `MemoryId` on the row (`memorySupersedes`, `MemorySuccessorUnique`), Goal supersession the prior `GoalId`. Index rows have no id at all (E5) |
| ST-5 | Index rows insert-only | `Immutable Edge`, `AppendOnly Edge`; re-assertion is a no-op by THEOREM `assert_present_row_changes_nothing` |
| ST-6 | source-ingest dedup key deterministic; duplicate = replay | excluded: source/flavor ingest metadata after D1; no core `FactReceiptId` entity |
| ST-7/8 | CitedObject/CitationMapping ids, insert-only, one mapping per citing memory | structural ids + scoped defs `CitedObjectIdUnique`/`CitationMappingIdUnique`/`CitationMappingUniqueBySubject`, `Immutable`/`AppendOnly` instances + theorem `citation_unique_per_subject` |
| ST-9 | Owner identity columns (principal kind + id) | `OwnerRef` is the stable stored owner reference (`world` / `personal u` / `group id`); `OwnerState.resolve` maps it to resolved `Owner := Group` for access. The exact SQL column shape is engine storage. org has no kernel face — decisions `2026-06-11-org-out-of-kernel.md`, owner realign 2026-06-28; no-op under 2026-07-06 User token change |
| ST-10 | Index-row ownership: source-owned rows; cross-owner targets always allowed when readable | row-validity predicate `EdgeSourceOwned` + THEOREM `cross_owner_target_admitted`. The Supersession carve-out is GONE because supersession is not an edge: it is a same-owner row pointer by construction (ME-5b). Target erasure/visibility affects `edge_target_redacted`, not `edge_owner` |
| ST-11 | INSERT-only cognitive lifecycle | class `AppendOnly` + instances |
| ST-13 | Only compliance erasure deletes | Compliance.lean: def `abandoned` is the SOLE delete trigger (owning group empty) + THEOREMs `drop_personal_abandoned`, `source_abandoned_cascades_to_edge`, `target_abandoned_does_not_abandon_source_owned_edge`, `world_never_abandoned` — target abandonment redacts/suppresses target projection only |
| ST-14 | Stateful current-state = head query, never replacement | `FactEntity` carries `current : Fact`; `FactEntityNaturalKeyUnique` is the natural-key table guard; Fact rows remain immutable observations |
| ST-15..17 | Vector-store independence (targets F/A/P AND Goals) | structural ABSENCE: no kernel `Embedding` entity, no `Memory → Embedding` accessor — embeddings are engine-side (`EmbeddingTarget`/`Embedding`/`embedding_target` retired 2026-06-28; the invariant was always the absence, never the declared type) |
| ST-22/23 | Content hash/dedup key not Fact identity; collision semantics | `Memory.id` remains Fact identity; `FactEntityId` is a fresh `Id` surrogate and natural key is only a uniqueness guard; source/flavor ingest dedup key excluded after D1 |
| ST-FE | FactEntity endpoint alignment | `NodeRef.factEntity` + `EndpointKind.FactEntityHead` + THEOREM `factEntityEndpointIsFact`; `NodeRefInTables` requires admitted FactEntity endpoints to have admitted current Fact heads. The `EndpointBinding` Pin/FollowHead pair is RETIRED: the address form IS the binding (`NodeRef.endpointKind`), so the two can no longer disagree and there is no alignment predicate left to check |
| ST-26 | Supersession logical; current state = query | defs `memorySupersedes`/`memoryIsHead`/`memoryHeads`/`perspectiveHeads` over the row's lineage pointer, table-scoped `goalIsHead`/`activeGoals` pattern + projection theorems `memory_superseded_not_head`, `perspective_head_is_perspective`, `perspective_head_is_memory_head` |

## 11 — Citations (CI)

| ID | Invariant | Carrier |
|---|---|---|
| CI-1 | Citation is Fact ∪ Abstraction (citation ⇒ not a Perspective; OPTIONAL since 2026-06-13) | subtype `Citable := { m // kind = .Fact ∨ kind = .Abstraction }` + structural `CitationMapping.subject : Citable`; THEOREMs `citation_subject_is_citable`, `citation_perspective_never_cites`, `citation_implies_citable`, `citation_pointer_never_on_perspective` over table-scoped choice-def `memory_citation`. WIDENED by doc 16 §Computed Scores Are Abstractions: a persisted computed score is a claim, so it is an Abstraction citing its computation record |
| CI-2 | At most one mapping per citing memory; subject is Fact ∪ Abstraction; no orphans | validity predicate `CitationMappingUniqueBySubject`; THEOREMs `citation_points_back`, `citation_points_to_row`, `citation_reverse_total`, `citation_unique_per_subject`. Multiplicity stays 0..1 |
| CI-3 | A/P cite transitively via provenance | table-scoped `GroundsInFact` / `memory_grounds_in_facts` over `MemoryGraphValid`; closure now terminates at Fact citations AND direct Abstraction citations (doc 16), and descends along both index kinds — an interpretation Perspective grounds through its references |
| CI-7/8 | Owner scoping; citing memory's owner = object owner | structural field `CitationMapping.owner_match`; THEOREM `citation_owner_match` |
| CI-9 | One object ↔ N mappings | structural absence of object-side restriction |
| CI-12/13 | Edges do not cite | structural absence of a citation accessor on `Edge` — and now of any accessor beyond the key and the owner (E6) |
| CI-14 | Operator provenance = row metadata, not citation | comment Operators.lean |
| CI-4/5/6/10/11 | content-hash idempotency mechanics | excluded: engine (hash invisible below opacity boundary) |
| CI-15/16 | S3 bytes, presigned URLs | excluded: engine storage |
| CI-18 | An upload is one write: the artefact, its typed row, the citation, the Fact and any flavor extension commit together or not at all | excluded: the kernel models a set of rows — no partial state, no write ordering, no transaction. Same stance as CN-9 |
| CI-19 | A Fact may decline a vector, and the declination survives every repair path | excluded: the kernel has no embedding vocabulary at all. ST-15..17 makes the structural ABSENCE of an `Embedding` entity the invariant, which is the opposite direction from "this particular Fact has no vector" |

## 13 — Compliance (CO)

| ID | Invariant | Carrier |
|---|---|---|
| CO-1 | Cognitive lifecycle append-only | classes + instances (ST-11 row) |
| CO-7 / ST-13 | Erasure = ABANDONMENT (reference count zero): an entity whose owning group has no members is wipeable; a user dropping abandons their personal group | def `abandoned` (`∀ u, o u = none`) + THEOREM `drop_personal_abandoned` over `Group.drop`; source cascade THEOREM `source_abandoned_cascades_to_edge`; target erasure/redaction THEOREMs `target_abandoned_redacts_edge_target`, `target_abandoned_does_not_abandon_source_owned_edge`; retention boundary THEOREM `world_never_abandoned`. The cascade now reads `EdgeValid` rather than a registry-scoped witness. Realign 2026-06-28 — replaces axioms `erased`/`erasure_removes_cognitive` + THEOREM `erasure_removes_edges` |
| CO-2/3/4/5/6, CO-8, CO-12/13/14 | Admin op surface / scope / outcomes / source-scope delete | excluded: admin op & outcome protocol — the kernel rule is the `abandoned` predicate, not an op/outcome enum (`ComplianceOp`/`ComplianceOutcome`/`DeleteSourceScope` retired 2026-06-28) |
| CO-9/10 | Pause/resume semantics | excluded: runtime dispatch gate, NOT erasure (`paused` axiom retired 2026-06-28) |
| CO-15/16/17/18/20 | Suppression / dedup-key retention + re-ingest block | excluded: source/flavor ingest boundary (`SuppressionKey`/`SuppressionEntry`/`suppression_key` retired 2026-06-28) |
| CO-19/29 | Suppression/audit survive erasure indefinitely | excluded: audit & suppression are engine tables, never cognitive rows — they survive by not being kernel entities at all |
| CO-11, CO-21..28, CO-30..58 | Export, audit content, side effects, vocabulary fields, owner policy, GDPR mappings | excluded: controller/engine obligations and legal commentary; ES-9 carries the kernel-relevant face |

### r1 additions (codex review, 2026-06-11)

| ID | Invariant | Carrier |
|---|---|---|
| GO-2b | Stale prior cannot be lifecycle head / one successor per prior id | table validity `GoalSuccessorUnique`; THEOREMs `goal_supersession_prior_is_head`, `goal_superseded_not_head` |
| GO-15 | Goal title/text core retrieval text | structure fields + accessors `goal_title`, `goal_text` |
| GO-16 | Operator-authored Goals do not carry materialized authoring personality | structural absence: no `goal_authoring_personality`; evidence is the `Goal.evidence` column, and the `reference` index rows follow from it |
| CI-17 | Cited objects / mappings schema-registered | structure fields `cited_object_schema`/`citation_mapping_schema : SchemaRef` (schema-typed); registration is engine admission (D16) |
| ST-EdgeId | source-ingest edges content-hash id vs UUIDv7 | RETIRED with the column it constrained. There is no edge id: `EdgeId`, `ContentHash`, `EdgeUuid`, `EdgeIdAuthorshipValid` and `edge_id_authorship_split` are all deleted, and E5 (`edge_key_determines_row`) gives structurally what the identity hash approximated. The row that carried this cell also carried the only reference to a numbered "AGENTS.md invariant 17" — no such numbered ledger exists there, so the anchor is dropped rather than repointed |

## Principle surface map

Principles aggregate multiple ID rows above under one named surface property
(cross-cutting summaries, not independent coverage rows), so this table's
`Runtime enforcement` values are ROLLUPS of the named constituent ID rows —
the constituent row is the authoritative cell; a principle whose parts differ
says `mixed` and names which part is which.

| Principle | Named surface prop | Kernel carrier |
|---|---|---|
| P1 | `principle_1_facts_below_perspective` | `MemoryKind.layer` theorem: Fact layer below Perspective. |
| P2 | `principle_2_operator_goals_carry_evidence` | `GoalEvidenceValid` over the Goal row's `evidence_memory_ids`; WEAKENED to operator-derived Goals only, with goal measurement/justification left to a decider. |
| P3 | `principle_3_operators_never_output_facts`; `principle_3b_goal_close_is_an_act`; `principle_3c_causal_closure_is_perspectival`; `principle_epistemic_operator_output_not_fact` | `operator_memory_output_not_fact` over `InvocationShapeValid`; `terminal_goal_closes_with_fact` + `goal_close_fact`; P3c REBASED — `goal_declared_rows_are_references` (a Goal declares only references, and the kind vocabulary has no causal variant, so the attribution cannot be an observer-independent edge; the judgment form of it is an interpretation node); epistemic corollary names the induction-as-representation bound (not Hume solved). |
| P4 | `principle_4_facts_connect_non_interpretively`; `principle_epistemic_edge_kinds_are_exactly_two`; `principle_epistemic_fact_never_interprets`; `principle_epistemic_supersession_cannot_touch_facts` | REBASED on the two-kind model: `fact_source_reaches_only_facts` (E3) keeps a Fact source below every higher layer; `principle_epistemic_edge_kinds_are_exactly_two` exhausts the closed vocabulary, so no Fact→Fact row can carry a causal or interpretive value; `interpretation_is_never_a_fact` puts the claim in a Perspective; `facts_never_supersede` keeps Facts out of lineage. |
| P5 | `principle_5_memories_grounded_in_facts`; `principle_epistemic_abstraction_grounded_in_facts`; `principle_epistemic_perspective_is_no_view_from_nowhere` | `MemoryGraphValid` bundles memory/goal/FactEntity/index table validity, FactEntity head presence, E1 endpoint presence, memory-supersession validity, derived-row provenance, and strict derivation time; `memory_grounds_in_facts`, `abstraction_grounds_in_facts`, and `perspective_has_provenance` prove admitted rows bottom out in Facts. The Perspective corollary is RENAMED and WEAKENED — a Perspective now names an admitted memory of any kind, not necessarily an Abstraction (see CN-6). |
| P6 | `principle_6a_derivation_provenance_strictly_upward`; `principle_6b_personality_read_scope_removed` | `edge_layer_rule` over `EdgeValid`; structural absence of `read_scope`/`personality_may_read`; wake context deferred. |
| P7 | `principle_7_personality_is_not_entity` | structural absence: no personality row/type/instance; no Personality module; Self projections are queries over existing Goal/Perspective rows. |
| P8 | `principle_8_knowledge_artifact_model_independent`; `principle_8b_long_term_knowledge_artifact_has_text_memory` | `KnowledgeArtifact` + `InterpreterClass` witness semantic uptake at class level; `KnowledgeArtifactIn` proves admitted text-bearing Memory carrier. |
| P9 | `principle_9_index_is_a_function_of_node_content` | `rebuilt_table_valid` — the v0.0.8 master invariant: a store whose node declarations are layer-legal rebuilds into a valid index, so E2 and E3 are consequences of E7 rather than gates run after the fact. |

Principle decider exclusions:
- P2 goal measurement/justification.
- P7 emergent Personality semantics beyond structural absence.
- P6 wake/read conditioning mechanism.
- P6 future-only context evolution.

Parked design choices:
- P2 goal-row-total evidence bridge deliberately not added; the spec weakened
  P2 to operator-derived Goals only.
- P6 matrix-version axis removed with materialized read-scope; wake context
  evolution remains deferred.
