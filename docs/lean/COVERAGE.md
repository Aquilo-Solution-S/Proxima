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
| U-2 | Causal claims perspective-relative; no semantic/causal Fact→Fact; Perspective is locus of causal claims | REBASED on the v0.0.7 model, in two parts. (a) STRUCTURAL: the closed `EdgeKind` has no Causal/Interpretive variant, so there is no row a causal claim could occupy — THEOREM `principle_epistemic_edge_kinds_are_exactly_two` exhausts the vocabulary. (b) DEFINITIONAL + THEOREM: `interpretationOf` DEFINES an interpretation as a Perspective whose payload references its subjects, and `fact_source_reaches_only_facts` (from E3) proves a Fact source can never reach an Abstraction or Perspective to interpret it. The definitional half is stated as such — with the class matrix retired there is no matrix cell left to prove it from, and pretending otherwise would be a decoy |
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
| AUTH-EDGE | Pin write admission = source write + target read | def `pin_write_admitted`; THEOREMs `pin_write_admitted_source_write`, `pin_write_admitted_target_read`, `cross_owner_target_admitted` |
| AUTH-EDGE-READ | Pin read is source-local; target projection is separately gated | defs `pin_source_read_admitted`, `pin_target_readable`; render `pin_render_hot` / `pin_render_cold` / `pin_render_unavailable` |
| NEST-1 | Group nesting needs no new primitive — a nested group resolves to an ordinary `Owner` | def `Role.meet`/`Role.join` (role lattice) + `Group.mount`/`Group.union`; the kernel sees only the resolved `Owner`, nesting is host composition (Level 2, 2026-06-28) |
| NEST-2 | Capped mounting cannot escalate write authority | THEOREM `mount_cannot_escalate` — if the cap may not write kind `k`, no member of the mounted group gains write `k` (meet caps the write ceiling) |
| NEST-3 | Union grants at least each side's access | THEOREM `union_grants_each` — read via either group ⇒ read via the union (join never lowers a member's capability); write case analogous |

## 02 — Memory (ME)

| ID | Invariant | Carrier |
|---|---|---|
| ME-1 | Fact is Memory with kind `.Fact` | subtype `Fact := { m : Memory // memory_kind m = .Fact }` + theorem `fact_memory_kind` |
| ME-2 | Fact owner is the memory row owner | structural: `Fact.memory` projects to `Memory.owner` |
| ME-3 | Optional free text is a Memory field for F/A/P | RETIRED: prose lives on the sidecar (`KnowledgeArtifact.text`), not `Memory`. THEOREM `knowledge_artifact_has_text` |
| ME-4 | Facts never supersede / never superseded | RETIRED: later `t` on a Fact handle is a new observation. THEOREM `fact_origins_nothing` / `facts_declare_no_origins` — a Fact declares no origins |
| ME-5a | Supersession same kind | RETIRED with `Memory.supersedes`. Series kind is frozen on `MemoryHead` (`MemoryHeadAligned`) |
| ME-5b | Supersession same owner | RETIRED with `Memory.supersedes`. Series owner is frozen on `MemoryHead` (`MemoryHeadAligned`) |
| ME-6 | Personality is not a materialized Memory author/owner slot | structural absence: no `PersonalityInstance`, no `personality_owner`, no `memory_authoring_personality`; D4 comment Memory.lean |
| ME-7 | Facts below Perspectives; no personality read-scope matrix | theorem `principle_1_facts_below_perspective`; structural absence of `read_scope`/`personality_may_read`; wake trigger/context reads use `Wake.Firing.trigger_read` and `each_injected_read` over actual memory owners |
| ME-8 | Materialized personality matrix removed | structural absence of `read_scope` and matrix-version state; wake context/read semantics are role-graded Owner checks in `Wake.Firing`, not a personality matrix |
| ME-9 | Pins live on the declaring row (E2); no edge owner column | structural: `origins`/`refs` are Memory fields. Write admission `pin_write_admitted` (source write + target read). THEOREM `cross_owner_target_admitted` |
| ME-10 | ℓ(source) ≥ ℓ(target) for origins | def `OriginKindValid` + THEOREM `origin_layer_rule`; Fact origins empty (`fact_source_reaches_only_facts`) |
| ME-11 | Class-legality matrix (9 cells) | RETIRED. Layering is `OriginKindValid` / `origin_layer_rule` |
| ME-12 | Supersession same endpoint shape (incl. Goal→Goal) | RETIRED with supersedes. Later `t` on the same handle stays on that entity axis (`Handle` vs `MemoryId`/`GoalId`) |
| ME-13 | Index rows immutable v1 | RETIRED: there is no Edge table. Pins are fields of an append-only Memory/Goal row |
| ME-14 | Descriptor masks tighten, never relax | RETIRED: no descriptors. `OriginKindValid` is checked on the declaring node |
| ME-15 | Causal chain is a query, not an entity; materialized = cache only | structural absence + Edges.lean header (the chain is now: reference backbone + interpretation Perspectives + origin closure) |
| ME-16 | Memory id is identity | structure field `Memory.t` (`memory_t` / `memory_id`) + `MemoryIdUnique`. Series is `Memory.handle`. Shared `ContentId` does not collapse `t` — THEOREM `shared_content_preserves_distinct_admissions` / `principle_content_share_preserves_t` |
| ME-CONTENT | Typed payload is owner-scoped `Content`; A/P name a ContentId; `(owner, schema, hash)` unique | structure `Content`; defs `ContentIdUnique`, `ContentKeyUnique`, `ContentAligned`, `contentShared`; `Memory.content_id` |
| ME-17 | Personality is emergent from Perspective/wake context, not a stored instance | structural absence in Memory.lean/Principles.lean; no Personality module; `ownerPerspectives` is the candidate pool; Self is `situatedSelf` |
| ME-18 | Cross-context supersession policy | excluded: wake/Perspective context semantics deferred after D4; no personality instance axis in kernel |
| ME-19 | Relation registry: unregistered relations invalid | RETIRED. E4 is structural (`origins`/`refs`) + `pins_are_node_content` |
| ME-20 | Core relations table (derived-from/supersedes/inspires/authored) | RETIRED: `derived-from` → `Memory.origins`; `points-at` → `Memory.refs`; `inspires` → `Goal.assignment_t`; `depends-on` → `Goal.dependency_t`; `motivated-by` → `Goal.evidence_t`; authorship → write-act Fact (`Goal.write_act_t` / produced `refs`). No supersedes |
| ME-K1 | Text-bearing Memory rows can be model-independent knowledge artifacts | REBASED: text is sidecar. `KnowledgeArtifact.text` + THEOREMs `knowledge_artifact_has_text`, `knowledge_artifact_model_independent`, `knowledge_artifact_recoverable_by_its_kind` |
| ME-K2 | Long-term knowledge artifact = admitted text-bearing Memory row, not one model cache | def `KnowledgeArtifactIn memories artifact`; THEOREM `long_term_knowledge_artifact_has_text_memory`; no `Truth`/`Knows`/specific LLM or human instance in core |

## 16 — Edges (E) — REBASED: no Edge table (v0.0.8)

Pins live on the node (`Memory.origins` / `Memory.refs`; Goal `*_t` columns).
There is no `Edge` / `NodeRef` / `FactEntity` type. `#guard_msgs` pins the
headline pin theorems in `Causa/Edges.lean`.

| ID | Invariant | Carrier |
|---|---|---|
| E1 | Existence — every pinned `t` exists | def `pinExists` (hot `Memory.t` or `Cooled` stub) + `MemoryGraphValid.pinTargetsExist` |
| E2 | Ownership — the pin is on the declaring row | structural (no edge owner column) + `pin_write_admitted` / `cross_owner_target_admitted` |
| E3 | Layering — UML origin CHECKs | def `OriginKindValid` + THEOREM `origin_layer_rule`; THEOREM `fact_source_reaches_only_facts` |
| E4 | Kind follows operation; no free-standing pin write | STRUCTURAL: `origins` vs `refs` are the two fields. THEOREM `pins_are_node_content`. No verb writes a pin |
| E4z | A write with ZERO origins is legal | THEOREM `declaration_without_origins_writes_no_origin_pins` + `interpretationOf`; THEOREM `invocation_without_inputs_is_complete` |
| E5 | Structural idempotency — no pin row | STRUCTURAL ABSENCE of `Edge` / `EdgeId`. The pin set is the node's arrays |
| E6 | No content — no pin payload | STRUCTURAL ABSENCE: arrays of `MemoryId` only |
| E7 | Rebuildability — the pin set IS node content | def `derivePins` + THEOREMs `pins_are_node_content`, `derived_table_rebuildable`, `principle_9_index_is_a_function_of_node_content` |
| E-KIND | Two kinds, closed, not flavor-extensible | inductive `EdgeKind` (`origin`, `reference`); THEOREM `principle_epistemic_edge_kinds_are_exactly_two` |
| E-NODE | Interpretation is a node, not a kind | def `interpretationOf` + THEOREMs `interpretation_is_never_a_fact`, `interpretation_rows_are_references` |
| E-TIME | `created_at` on an edge row | RETIRED: no edge row. Kernel time axis is `Memory.tick` (uuidv7 `t` order), not a `created_at` column |

## 04 — Consolidation (CN)

| ID | Invariant | Carrier |
|---|---|---|
| CN-1 | F→A input/output shape | `OperatorPhase.inputKind`/`outputMemoryKind` + `InvocationShapeValid`; THEOREMs `operator_inputs_match_phase`, `invocation_output_memory_kind_valid`. The edge-shape carrier is retired: there is no authorship column to match a phase against, and the origins a write declares ARE its inputs |
| CN-2 | A→P input/output shape | same carrier: `InvocationShapeValid` + `operator_inputs_match_phase`; the output kind is `.Perspective` by `OperatorPhase.outputMemoryKind` |
| CN-3 | A→Goal evidence shape | `OperatorPhase.outputGoalAllowed` + `InvocationProvenanceComplete.goalInputs` (a Goal output declares its inputs as `reference` rows, because a Goal rests on them rather than deriving from them) + `OperatorPhase.inputEdgeKind` |
| CN-4 | frame — `P × A_cross → P` | RETIRED as an edge shape. A frame is a Perspective whose payload references the cross-domain Abstraction (doc 02 §The Layering Principle), i.e. an ordinary `reference` declaration; `interpretationDeclaration` is its constructive witness and `PerspectiveLink` has no successor |
| CN-5 | No downward writes | THEOREM `operator_memory_output_not_fact` (from the phase output contract alone — F→A/A→A give Abstraction, A→P gives Perspective, A→Goal gives no memory row) + THEOREM `operator_origin_row_not_upward` (E3 on the ledger's own rows) + ME-1 Fact subtype |
| CN-6 | Derived memories have valid provenance | `MemoryGraphValid.derivedProvenance` + `abstractionHasOrigins`. THEOREM `abstraction_has_provenance` (nonempty origins). `perspective_has_provenance` (origins or refs nonempty) |
| CN-7 | Cross-domain join is typed Abstraction | comment (shape carried by CN-6 + U-2 matrix) |
| CN-8 | F→A batch-gate exclusivity per (owner, batch, input contract, operator, output schema) | RETIRED: no source-batch / operator / input-contract columns on Memory. Visit is a Fact + refs. Recipe, if any, is sidecar |
| CN-8b | Operator invocation input completeness | `OperatorInvocation` + `InvocationProvenanceComplete`: memory outputs name inputs in `origins`; Goal outputs name them in `evidence_t`. THEOREM `invocation_without_inputs_is_complete` (E4z) |
| CN-9 | Atomic invocation (all-or-nothing outputs) | excluded: storage-layer transaction contract (same stance as WH event/projection atomicity); Lean only validates an admitted invocation ledger/manifest |
| CN-10 | Retry/changed-prompt = new derivation, never mutation | `AppendOnly Memory` + comment Operators.lean |
| CN-11 | Wake dispatcher loop, cursors, depth bound, runtime tables | excluded: engine runtime |
| CN-12 | Prompt locality (core ships no domain prompts) | excluded: engine/flavor split mechanics; spirit carried by CF-A |

## 06 — Goals (GO)

| ID | Invariant | Carrier |
|---|---|---|
| GO-1 | Later `t` on a handle keeps owner | table validity `GoalTransitionValid`; projection THEOREM `goal_transition_same_owner` |
| GO-2 | Valid lifecycle transition | def `goalTransitionAdmitted` + `goalImmediatelySucceeds` + `GoalTransitionValid`; projection THEOREM `goal_transition_admitted` |
| GO-3 | Goal DAG acyclic | `Goal.dependency_t` pins other Goal `t`. Acyclicity is engine validation, not a Goal-row invariant |
| GO-4 | Parents same owner | retired with Goal-local parents; the derived rows are source-owned by the declaring Goal (E2) and layer-exempt (Goal endpoints carry no layer), with no descriptor mask left to consult |
| GO-5 | Every transition new row; no in-place mutation | `AppendOnly Goal`; later `t` on the same handle is the new version; `GoalTerminalClosed` forbids later `t` after terminal |
| GO-6 | Goal is not Memory | structural: distinct Types |
| GO-7 | Self is a query, never an entity/cache; not parameterless | structural absence + `situatedSelf` (cue-indexed) + THEOREMs `situated_self_touches_cue`, `situated_self_subset_owner_perspectives`, `principle_situated_self_touches_cue`. `ownerPerspectives` / `selfPerspectives` is the candidate pool, not Self |
| GO-8 | Active set definition (heads, state=Active) | table-scoped defs `goalIsHead` (via `GoalHead`), `activeGoals`; series traversal `GoalSeriesReachable` + `activeGoalHeadFrom` |
| GO-9 | Goal id is identity | table validity `GoalIdUnique` on `Goal.t`; projection THEOREM `goal_id_injective`. Series is `Goal.handle` |
| GO-10 | Authorship vocabulary | RETIRED: no authorship blob. Write-act is `Goal.write_act_t` → a Fact `t` |
| GO-11 | GoalWrite protocol (request_id idempotency, conflict detection, stream visibility) | excluded from Goal ontology: request-id/body replay is protocol/write-atom state (doc 14), not a Goal row invariant; item 10 resolved by keeping it out of `Goal`/Self |
| GO-12 | Assignment is `Goal.assignment_t`; instance-scoped active_goals query | `goalAssignedToPerspective` + `activeGoalsForSelf` follows `GoalSeriesReachable`; THEOREMs `goal_assignment_target_perspective`, `active_goal_for_self_*` |
| GO-13 | Goal-scoped wake policy; planner-first | `Goal.wake_id : Option WakeId` + reusable `WakeConfig` row; `Wake.Firing.wake_config` binds `goal_wake_id = some config.wake_id`; `actor_member` is any server-resolved role in the Goal owner; `each_action_allowed` pins invoked Actions to `WakeConfig.toolset` |
| GO-14 | Goal assignment/evidence scope | `Goal.assignment_t` / `evidence_t`. `GoalEvidenceValid` resolves each evidence `t` to a hot or cooled non-Perspective. THEOREM `goal_evidence_not_perspective`. Operator-must-have-evidence RETIRED with authorship |
| GO-17 | Root Goal creation shape | `GoalRootValid` + THEOREM `goal_root_active`: least-tick version on a handle is Active only |
| GO-18 | Terminal close Fact table validity | `GoalTerminalCloseFactValid`: close Fact is a hot or cooled Fact with the same Owner |

## 03 — Schema Registry (SR) — in-kernel rows

| ID | Invariant | Carrier |
|---|---|---|
| SR-1 | Registry frozen at startup, no runtime registration | partial: the relation registry is GONE (nothing to freeze — the kind follows the operation); wake `Action` values are allowed only by the Goal's `WakeConfig.toolset` (`wake_invoked_actions_allowed`), while concrete schema/tool/source/prompt registry freeze and flavor linking remain build-time engine mechanics (Composition.lean deleted 2026-06-28, D16) |
| SR-2 | Every memory payload schema-typed | structure field `Memory.schema : SchemaRef` (accessor totality — every row carries an opaque schema tag). Schema registration in the active registry remains engine admission, not yet a kernel rule (D16) |
| SR-8 | Schema ids flavor-qualified | excluded: namespacing is engine id-minting (collision-freedom = "the engine mints distinct ids"), not kernel ontology (D16) |
| SR-11/16 | F/A/P may carry optional free text; sidecars may carry opaque typed payload | REBASED: no `Memory.text`. Sidecar text is `KnowledgeArtifact.text`. `Flavor.OptionalMemorySidecar` / `OptionalGoalSidecar` remain optional wrappers. No edge sidecar — there is no Edge type |
| SR-13 | Fact identity ≠ payload hash; UUIDv7 | structure field `Memory.id` + table/store invariant `MemoryIdUnique` + ST-22 comment |
| SR-14/44 | Fact has no supersedes | RETIRED as lineage. THEOREM `facts_declare_no_origins` — Fact origins are empty |
| SR-24 | edge kind closed, not flavor-extensible | inductive `EdgeKind` — two variants (`origin`, `reference`), not five classes under an open relation vocabulary. The `relation_class` enum, its namespaced relation ids, and `EdgePayload` are all deleted (doc 16 §Kinds are closed); a feature that seems to need a third kind is missing a node |
| SR-25 | Index rows immutable v1 | `Immutable Edge` |
| SR-30..33 | special_category per schema, author-declared | excluded: GDPR controller/engine concern (`schema_special_category` cut 2026-06-28, D16) — the kernel never reasons over special-category |
| SR-43 | Stateful Fact: each observation a new Fact | REBASED: same `handle`, later `t`. No `FactEntity`. Head is `MemoryHead.t` |
| SR-46 | Tombstone is a Fact (deletion is observed state) | comment Identity.lean / excluded detail: stateful-schema mechanics |
| SR-49 | Memory row stores schema id+version+kind | REBASED: `schema` lives on `MemoryHead` only (`memory_head_schema`). Kind is on the row and the head (`MemoryHeadAligned`). No `schema_version` |
| SR-56/57 | Layer/kind change in migration forbidden | excluded: migration mechanics; spirit = kind is fixed per memory (accessor totality, immutability) |

## 08 — Core & Flavors (CF) — in-kernel rows

| ID | Invariant | Carrier |
|---|---|---|
| CF-G | Payload opacity — the domainless boundary | structural ABSENCE: `SchemaRef` (Identity) is an opaque per-row tag with NO accessor at all (no resolution, no payload, no capabilities); `Flavor.OptionalMemorySidecar` / `OptionalGoalSidecar` prove sidecar payload changes do not affect kernel-visible rows. Opacity is also why `NodeDeclaration` carries resolved endpoints rather than payload fields: WHICH payload fields are reference fields is schema-registry knowledge below the boundary |
| CF-* (compliance) | A flavor must comply with the basic rules | THEOREMS in `Causa.Flavor` — a concrete flavor's rows discharge the universal invariants (`fact_is_fact`, `perspective_is_perspective`, `abstraction_grounded`, `flavor_perspective_has_provenance`, `flavor_declared_edges_valid`, `published_readable`, `published_read_only`, `wipeable_when_abandoned`) via only pre-existing theorems; the rules quantify over every row, so compliance is derived, never axiomatized |
| CF-OPEN | Substrate is OPEN — any app integrates as a flavor with zero kernel change and no new Causa axiom | `Causa.Flavor` constructive witness: a flavor's vocabulary is inhabitants of the existing opaque type `SchemaRef`, taken as a PARAMETER; `#print axioms` on the flavor theorems names no Causa axioms — never one named `flavor`. That machine-checked absence IS the openness (D16/D18). NARROWED in v0.0.7: `RelationId` is gone, so a flavor can no longer mint traversable link vocabulary — the deliberate loss recorded in doc 16 §What This Removes. The escape valve is an interpretation node, and it is total |
| CF-43..46 | Goal entity core-owned; flavor owns payload/tools | structural: Goal lives in the kernel; payloads opaque |
| CF-60 | Cross-flavor reads obey Owner/access surface | Owner/read authorization (Causa.Authorization); no materialized personality read-scope after D4 |

## 07 — Storage (ST) — in-kernel rows

| ID | Invariant | Carrier |
|---|---|---|
| ST-1..4 | Fresh ids; immutable identity; new version = new `t` | `Memory.t` + `MemoryIdUnique`, `Goal.t` + `GoalIdUnique`, classes `Immutable`/`AppendOnly`. Later `t` on the same handle is the new version. No Edge id (E5) |
| ST-5 | Index rows insert-only | RETIRED: no Edge table. Pins are fields of append-only node rows |
| ST-6 | source-ingest dedup key deterministic; duplicate = replay | excluded: source/flavor ingest metadata after D1; no core `FactReceiptId` entity |
| ST-7/8 | Blob ids, insert-only, 0..1 citation per memory | `Blob` + `BlobIdUnique` + `Memory.blob_id : Option BlobId`; THEOREM `citation_unique_per_subject`. No mapping table |
| ST-9 | Owner identity columns (principal kind + id) | `OwnerRef` is the stable stored owner reference (`world` / `personal u` / `group id`); `OwnerState.resolve` maps it to resolved `Owner := Group` for access. The exact SQL column shape is engine storage. org has no kernel face — decisions `2026-06-11-org-out-of-kernel.md`, owner realign 2026-06-28; no-op under 2026-07-06 User token change |
| ST-10 | Index-row ownership: source-owned rows; cross-owner targets always allowed when readable | row-validity predicate `EdgeSourceOwned` + THEOREM `cross_owner_target_admitted`. The Supersession carve-out is GONE because supersession is not an edge: it is a same-owner row pointer by construction (ME-5b). Target erasure/visibility affects `edge_target_redacted`, not `edge_owner` |
| ST-11 | INSERT-only cognitive lifecycle | class `AppendOnly` + instances |
| ST-13 | Only compliance erasure deletes | REBASED: def `wipeable := abandoned ∨ (cold ∧ unreferenced ∧ policy)` + THEOREMs `wipeable_when_abandoned`, `wipeable_when_cold_unreferenced_policy`, `drop_personal_abandoned`, `world_never_abandoned`. Content: `contentWipeable := abandoned ∨ contentUnreferenced`. Forget cools; erase is abandonment-only |
| ST-14 | Stateful current-state = head query, never replacement | `MemoryHead.t` / `GoalHead.t` are display/search heads. Each `t` stays. No `FactEntity` |
| ST-15..17 | Vector-store independence (targets F/A/P AND Goals) | structural ABSENCE: no kernel `Embedding` entity, no `Memory → Embedding` accessor — embeddings are engine-side (`EmbeddingTarget`/`Embedding`/`embedding_target` retired 2026-06-28; the invariant was always the absence, never the declared type) |
| ST-22/23 | Content hash/dedup key not Fact identity; collision semantics | Fact identity is `Memory.t`; `Content.hash` is payload identity within `(owner, schema)`; `ingest_keys` is the only sourced unique; THEOREM `shared_content_preserves_distinct_admissions` |
| ST-FE | FactEntity endpoint alignment | RETIRED: no `FactEntity`. Pins are Memory `t` or a cooled stub (`pinExists`). Follow-at-read is forbidden |
| ST-26 | Current state = head query | defs `memoryIsHead` / `memoryHeads` / `perspectiveHeads` over `MemoryHead`; `goalIsHead` / `activeGoals` over `GoalHead` |

## 11 — Citations (CI)

| ID | Invariant | Carrier |
|---|---|---|
| CI-1 | Citation is Fact ∪ Abstraction (citation ⇒ not a Perspective; OPTIONAL) | `Memory.blob_id` 0..1 + row fields `perspective_never_cites` / `blob_fa_only`; THEOREMs `citation_subject_is_citable`, `citation_perspective_never_cites`, `citation_implies_citable`, `citation_pointer_never_on_perspective`. No `CitationMapping` table |
| CI-2 | At most one citation per citing memory | STRUCTURAL: one `Option BlobId`. THEOREM `citation_unique_per_subject` |
| CI-3 | A/P cite transitively via provenance | table-scoped `GroundsInFact` / `memory_grounds_in_facts` over `MemoryGraphValid`; closure now terminates at Fact citations AND direct Abstraction citations (doc 16), and descends along both index kinds — an interpretation Perspective grounds through its references |
| CI-7/8 | Owner scoping; citing memory's owner = blob owner | def `memory_cites` + THEOREM `citation_owner_match` |
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
| CO-7 / ST-13 | Erasure = abandonment, or cold ∧ unreferenced ∧ policy | def `abandoned` + def `wipeable` (`abandoned ∨ (cold ∧ unreferenced ∧ policy)`) + THEOREMs `wipeable_when_abandoned`, `wipeable_when_cold_unreferenced_policy`, `drop_personal_abandoned`, `world_never_abandoned`. No edge cascade (no Edge table). Pin render: `pin_render_hot` / `pin_render_cold` / `pin_render_unavailable` |
| CO-2/3/4/5/6, CO-8, CO-12/13/14 | Admin op surface / scope / outcomes / source-scope delete | excluded: admin op & outcome protocol — the kernel rule is the `abandoned` predicate, not an op/outcome enum (`ComplianceOp`/`ComplianceOutcome`/`DeleteSourceScope` retired 2026-06-28) |
| CO-9/10 | Pause/resume semantics | excluded: runtime dispatch gate, NOT erasure (`paused` axiom retired 2026-06-28) |
| CO-15/16/17/18/20 | Suppression / dedup-key retention + re-ingest block | excluded: source/flavor ingest boundary (`SuppressionKey`/`SuppressionEntry`/`suppression_key` retired 2026-06-28) |
| CO-19/29 | Suppression/audit survive erasure indefinitely | excluded: audit & suppression are engine tables, never cognitive rows — they survive by not being kernel entities at all |
| CO-11, CO-21..28, CO-30..58 | Export, audit content, side effects, vocabulary fields, owner policy, GDPR mappings | excluded: controller/engine obligations and legal commentary; ES-9 carries the kernel-relevant face |

### r1 additions (codex review, 2026-06-11)

| ID | Invariant | Carrier |
|---|---|---|
| GO-2b | Stale prior cannot be lifecycle head / one successor per prior id | REBASED: head is `GoalHead.t`. `GoalTerminalClosed` forbids later `t` after terminal. `GoalRequestUnique` is replay |
| GO-15 | Goal title/text core retrieval text | REBASED: `goal_title` stays on the row; body text is sidecar (no `goal_text`) |
| GO-16 | Operator-authored Goals do not carry materialized authoring personality | structural absence; authorship blob RETIRED; write-act is `Goal.write_act_t` |
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
| P2 | `principle_2_goal_evidence_not_perspective` | `GoalEvidenceValid` over `evidence_t`; authorship-gated operator-must-have-evidence RETIRED with the authorship blob. |
| P3 | `principle_3_operators_never_output_facts`; `principle_3b_goal_close_is_an_act`; `principle_3c_causal_closure_is_perspectival`; `principle_epistemic_operator_output_not_fact` | `operator_memory_output_not_fact`; `terminal_goal_closes_with_fact` (`close_fact_t.isSome`); P3c — Goal pins live on the Goal row, never in `Memory.refs` (`goal_declared_rows_are_references`). |
| P4 | `principle_4_facts_connect_non_interpretively`; `principle_epistemic_edge_kinds_are_exactly_two`; `principle_epistemic_fact_never_interprets` | Fact origins are empty (`facts_declare_no_origins`); two closed kinds; `interpretation_is_never_a_fact`. `principle_epistemic_supersession_cannot_touch_facts` RETIRED with supersedes. |
| P5 | `principle_5_memories_grounded_in_facts`; `principle_epistemic_abstraction_grounded_in_facts`; `principle_epistemic_perspective_is_no_view_from_nowhere` | REBASED: `MemoryGraphValid` over memories/goals/`MemoryHead`/`Cooled` — no FactEntity, no Edge table. Descent is `origins`/`refs` (`pinFrom`) + `tick` well-founded. `GroundsInFact` bottoms out at Facts or cooled stubs. |
| P6 | `principle_6a_derivation_provenance_strictly_upward`; `principle_6b_personality_read_scope_removed` | `edge_layer_rule` over `EdgeValid`; structural absence of `read_scope`/`personality_may_read`; wake context deferred. |
| P7 | `principle_7_personality_is_not_entity` | structural absence: no personality row/type/instance; no Personality module; Self is `situatedSelf` over existing Perspective rows given a `Cue`. |
| P-CONTENT | `principle_content_share_preserves_t` | shared ContentId keeps distinct `t` |
| P-SELF | `principle_situated_self_touches_cue` | Self membership implies `cueTouches` |
| P8 | `principle_8_knowledge_artifact_model_independent`; `principle_8b_long_term_knowledge_artifact_has_text_memory` | `KnowledgeArtifact` + `InterpreterClass` witness semantic uptake at class level; `KnowledgeArtifactIn` proves admitted text-bearing Memory carrier. |
| P9 | `principle_9_index_is_a_function_of_node_content` | `derivePins` is identity: the pin set IS node content. No Edge table to rebuild. |

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
