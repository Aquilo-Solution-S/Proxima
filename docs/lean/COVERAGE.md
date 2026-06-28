# Kernel Coverage Matrix — docs → axioms

## universe.md (U)

| ID | Invariant | Carrier |
|---|---|---|
| U-1 | Layering strict & irreversible; no lower-layer memory from higher layer | theorem `fact_memory_kind` + theorem `operator_memory_output_not_fact` + `legalClasses` upward-False cells; header comments Memory.lean |
| U-2 | Causal claims perspective-relative; no semantic/causal Fact→Fact; Perspective is locus of causal claims | `legalClasses` Fact→Fact cell (no Causal/Interpretive) + `operatorEdgeShape` PerspectiveLink arm; Edges.lean header |
| U-3 | Dreaming = flavor-declared consolidation; no Dream entity/relation/pipeline | structural absence; Operators.lean header comment |
| U-4 | Reality enters as typed Facts | `Fact` subtype (`Memory` with kind `.Fact`) + `operator_memory_output_not_fact`; source/flavor ingest mechanics excluded per D1 |

## 01 — Source / Fact ingest (ES)

| ID | Invariant | Carrier |
|---|---|---|
| ES-1 | Group org → no kernel face | excluded: org has no kernel face and (Track B / S0) is absent from Core storage and identity — `Owner := Group` (a `Set User`); tenancy is a flavor/app concern. Decisions `2026-06-11-org-out-of-kernel.md`, S0 collapse, owner-ontology realign 2026-06-28 (was THEOREM `owner_org_denormalized` when Owner carried org) |
| ES-2 | Visibility rule (group membership) | def `visible` (`o r ≠ none`, i.e. holds any `Role`) + theorem `visible_personal` |
| ES-3 | org never enters access or identity | structural: org absent from the kernel entirely (`Owner := Group`, a `Set User`) — decisions `2026-06-11-org-out-of-kernel.md`, owner realign 2026-06-28 |
| ES-4 | source-ingest dedup key deterministic over source/owner/payload | excluded: source/flavor ingest metadata after D1; no core `EventId` entity |
| ES-5 | Batch id unique within (source, owner) | excluded: per-scope engine validation with no kernel-observable face; F→A gate carries owner dimension (`ftoa_batch_exclusive`); wake context dimension deferred after D4 |
| ES-6 | Facts are typed observations, not operator derivations | `Fact` subtype + `operator_memory_output_not_fact`; source trace metadata is flavor/ingest-side after D1 |
| ES-7 | 1:1 source-ingest receipt→Fact materialization | excluded: source/flavor ingest implementation; core sees only typed Fact rows after D1 |
| ES-8 | Source must not abstract/interpret/cross-join/relevance-filter/persist | excluded: source/flavor ingest contract; kernel carries the consequence via `operator_memory_output_not_fact` + no downward operator writes |
| ES-9 | Compliance metadata: every source declares 4 fields; Facts inherit | excluded: engine registration totality (CO-39..45 rows); source identity is engine-side — kernel `SourceId` axiom retired 2026-06-28 (unused once Compliance dropped `DeleteSourceScope`) |
| ES-10 | Idempotency keys content-derived/opaque, never natural-person identifiers | excluded: source/flavor ingest concern with no kernel carrier (suppression docstring retired 2026-06-28 with the `SuppressionKey` axioms) |
| ES-11 | Facts' typing frozen at insert; engine does not migrate Facts across schema versions | `Immutable Memory`-stance via `AppendOnly Memory` + accessor totality (`memory_schema` fixed per row); migration mechanics excluded (SR-50..55 exclusion block) |
| ES-12 | No `Principal.Org` variant; org-wide = `<org>-everyone` group | structural: no `Principal` sum at all — `Owner := Group` (`Set User`); a User is the singleton group `Owner.ofUser u`, org-wide is an ordinary shared (non-singleton) Group. Realign 2026-06-28 |
| ES-13 | Per-memory ACL (AccessGrant) is v2+, not v1 | structural absence + Owner.lean header comment |
| ES-14 | Push vs pull is source-side implementation detail | excluded: engine |
| ES-15 | Bootstrap/founding-goal is flavor onboarding | excluded: app-layer |
| AUTH-SHARE | One owner per entity; sharing IS group membership (no share set above the owner) | structural: an entity has a single `owner : Owner` (a Group); read-only share = viewer-role membership, publish = transfer to World. No `Scope`/`reaches`/`entity_owner` multi-owner table — realign 2026-06-28 |
| AUTH-READ | Read = role read-ceiling over the kind, in the entity's owning group | def `may_read` (`∃ x, o r = some x ∧ x.mayRead k`); role model `Role` (read/write ceilings over `AccessKind` F<A<P<G), `Role.mayRead` |
| AUTH-WRITE | Write = role write-ceiling in the single owning group | def `may_write` (`∃ x, o r = some x ∧ x.mayWrite k`); `Role.mayWrite` |
| AUTH-MANAGE | Meta-management = managing a group's membership/role map | def `may_manage` (`¬ Owner.isPersonal o ∧ ∃ x, o r = some x ∧ x.manages`); `Role.manage` flag + `Role.admin` preset |
| AUTH-MANAGE-P | Personal groups forbid meta-management | THEOREM `personal_forbids_manage`; structural — `may_manage` requires `¬ Owner.isPersonal`, and `personal.manage = false` |
| AUTH-MANAGE-W | World group forbids meta-management | THEOREM `world_forbids_manage`; structural — every World member is `viewer` (`manage = false`), independent of the personal-group rule |
| AUTH-1 | Write ⊆ read (whoever may write may read) | THEOREM `may_write_implies_read`, from structural field `Role.write_le_read` (same owning group) |
| AUTH-2 | World is read-only (never a write target) | THEOREM `world_read_only`, from `world := fun _ => some Role.viewer` (write ceiling 0) |
| AUTH-3 | World is universally readable | THEOREM `world_universally_readable`, from World's `viewer` read ceiling covering every kind |
| AUTH-4 | Per-memory ACL / owner-space grants absent — access is role-graded group membership | structural: no `AccessGrant`/`MemoryAction`; `Group := User → Option Role`; realign 2026-06-28 |
| AUTH-EDGE | Edge write admission = source write + descriptor-selected target gate | defs `edge_write_admitted`, `targetAccessSatisfied`; THEOREMs `edge_write_admitted_core_valid`, `edge_write_admitted_source_write` |
| NEST-1 | Group nesting needs no new primitive — a nested group resolves to an ordinary `Owner` | def `Role.meet`/`Role.join` (role lattice) + `Group.mount`/`Group.union`; the kernel sees only the resolved `Owner`, nesting is host composition (Level 2, 2026-06-28) |
| NEST-2 | Capped mounting cannot escalate write authority | THEOREM `mount_cannot_escalate` — if the cap may not write kind `k`, no member of the mounted group gains write `k` (meet caps the write ceiling) |
| NEST-3 | Union grants at least each side's access | THEOREM `union_grants_each` — read via either group ⇒ read via the union (join never lowers a member's capability); write case analogous |

## 02 — Memory (ME)

| ID | Invariant | Carrier |
|---|---|---|
| ME-1 | Fact is Memory with kind `.Fact` | subtype `Fact := { m : Memory // memory_kind m = .Fact }` + theorem `fact_memory_kind` |
| ME-2 | Fact owner is the memory row owner | structural: `Fact.memory` projects to `Memory.owner`; source/event owner inheritance moved out of core by D1 |
| ME-3 | Optional free text is a Memory field for F/A/P; no kind-based text axiom | structure field `Memory.text : Option Text` + accessor `memory_text` |
| ME-4 | Facts never supersede / never superseded | THEOREM `facts_never_supersede` (Edges.lean, from valid Supersession-class edge + matrix) |
| ME-5a | Supersession same kind | THEOREM `supersession_same_kind` over `EdgeCoreValid` rows |
| ME-5b | Supersession same owner | THEOREM `supersession_same_owner` over `EdgeCoreValid` rows, via descriptor `RelationOwnerPolicy.SameOwner` for Supersession |
| ME-6 | Personality is not a materialized Memory author/owner slot | structural absence: no `PersonalityInstance`, no `personality_owner`, no `memory_authoring_personality`; D4 comment Memory.lean |
| ME-7 | Facts below Perspectives; no personality read-scope matrix | theorem `principle_1_facts_below_perspective`; structural absence of `read_scope`/`personality_may_read`; wake read context deferred |
| ME-8 | Materialized personality matrix removed | structural absence of `read_scope` and matrix-version state; wake context/read semantics deferred after D4 |
| ME-9 | Edge scope source-owned; Supersession intra-Owner | row-validity predicate `EdgeSourceOwned` + descriptor owner policy `RelationOwnerPolicy`; projection THEOREMs `edge_source_owned`, `supersession_intra_owner` over `EdgeCoreValid` |
| ME-10 | ℓ(source) ≥ ℓ(target) for valid memory edges | THEOREM `edge_layer_rule` (from `EdgeCoreValid` + matrix empty upward cells) |
| ME-11 | Class-legality matrix (9 cells) | def `legalClasses` + THEOREM `edge_class_legal` (from `EdgeCoreValid` + descriptor `masksTightenOnly`) |
| ME-12 | Supersession same endpoint shape (incl. Goal→Goal) | row-validity predicate `EdgeSupersessionEndpointShapeValid` + THEOREM `supersession_same_endpoint_shape` |
| ME-13 | Edges immutable v1 | `Edge` structure + instances `Immutable Edge`, `AppendOnly Edge` |
| ME-14 | Descriptor masks tighten, never relax | structure `RelationDescriptor` with `endpointAdmitted` + proof field `masksTightenOnly`; row witness `EdgeValidWith`; projection THEOREM `edge_respects_mask` |
| ME-15 | Causal chain is a query, not an entity; materialized = cache only | structural absence + Edges.lean header |
| ME-16 | Memory id is identity | structure field `Memory.id` + table/store invariant `MemoryIdUnique` |
| ME-17 | Personality is emergent from Perspective/wake context, not a stored instance | structural absence in Memory.lean/Principles.lean; no Personality module; wake entries deferred |
| ME-18 | Cross-context supersession policy | excluded: wake/Perspective context semantics deferred after D4; no personality instance axis in kernel |
| ME-19 | Relation registry: unregistered relations invalid | excluded: relation registration is engine admission; the kernel governs relation CLASS legality (`legalClasses`, Edges), not id-registration (D16) |
| ME-20 | Core relations table (derived-from/supersedes/inspires/authored) | excluded: vocabulary content, not law — flavors/core register ids; classes pinned by `RelationClass` |

## 04 — Consolidation (CN)

| ID | Invariant | Carrier |
|---|---|---|
| CN-1 | F→A edge shape | def `operatorEdgeShape` + row-validity predicate `EdgeOperatorShapeValid`; full shape THEOREM `ftoa_edge_shape` |
| CN-2 | A→P edge shape | same merged carrier + `EdgeCoreValid`; full shape THEOREM `atop_edge_shape` |
| CN-3 | A→Goal evidence shape (Structural class) | `operatorEdgeShape` OperatorAtoGoal arm + `EdgeOperatorShapeValid` |
| CN-4 | frame/PerspectiveLink shape | `operatorEdgeShape` PerspectiveLink arm + `EdgeOperatorShapeValid` |
| CN-5 | No downward writes | theorem `operator_memory_output_not_fact` + `EdgeOperatorShapeValid` + ME-1 Fact subtype |
| CN-6 | Derived memories have valid provenance | axiom `derived_has_provenance` (merged, returns `EdgeCoreValid` edge); per-kind shapes THEOREMs `abstraction_has_provenance`, `perspective_has_provenance` |
| CN-7 | Cross-domain join is typed Abstraction | comment (shape carried by CN-6 + U-2 matrix) |
| CN-8 | F→A batch-gate exclusivity per (owner, batch, input contract, operator, output schema) | axiom `ftoa_batch_exclusive`; wake context dimension deferred after D4, no personality dim |
| CN-9 | Atomic invocation (all-or-nothing outputs) | excluded: storage-layer transaction contract (same stance as WH event/projection atomicity) |
| CN-10 | Retry/changed-prompt = new derivation, never mutation | `AppendOnly Memory` + comment Operators.lean |
| CN-11 | Wake dispatcher loop, cursors, depth bound, runtime tables | excluded: engine runtime |
| CN-12 | Prompt locality (core ships no domain prompts) | excluded: engine/flavor split mechanics; spirit carried by CF-A |

## 06 — Goals (GO)

| ID | Invariant | Carrier |
|---|---|---|
| GO-1 | Supersession same owner | table validity `GoalSupersessionResolved` + `GoalSupersessionValid`; projection THEOREM `goal_supersession_same_owner` |
| GO-2 | Valid lifecycle transition | def `goalTransitionAdmitted` + table validity `GoalSupersessionResolved` + `GoalSupersessionValid`; projection THEOREM `goal_supersession_admitted` |
| GO-3 | Goal DAG acyclic | retired from Goal row: no `goal_parents`; Goal↔Goal topology is Edge topology/relation validation |
| GO-4 | Parents same owner | retired with Goal-local parents; Edge ownership + descriptor masks govern Goal↔Goal relation legality |
| GO-5 | Every transition new row; no in-place mutation | `AppendOnly Goal`; supersession stores prior `GoalId` and current state is a table query |
| GO-6 | Goal is not Memory | structural: distinct Types |
| GO-7 | Self is a query, never an entity/cache | structural absence + Goals.lean header |
| GO-8 | Active set definition (heads, state=Active) | table-scoped defs `goalIsHead`, `activeGoals` |
| GO-9 | Goal id is identity | table validity `GoalIdUnique`; projection THEOREM `goal_id_injective` |
| GO-10 | Authorship vocabulary | inductive `GoalAuthorship` |
| GO-11 | GoalWrite protocol (request_id idempotency, conflict detection, stream visibility) | excluded: protocol surface (doc 14 out of scope per spec) |
| GO-12 | Assignment = causal `core/inspires` edge Goal→Self-Perspective; instance-scoped active_goals query | relation class `Causal`; `PerspectiveGoalLink` admits Goal→Perspective inspiration; traversal query engine-level per decision `2026-06-11-active-goals-two-queries.md` |
| GO-13 | Goal-scoped wake policy; planner-first | excluded: engine runtime |
| GO-14 | Goal assignment/evidence scope | Goal rows carry Owner; Goal↔Goal assignment/evidence is Edge topology governed by source-owned edge ownership + descriptor/write-surface policy |

## 03 — Schema Registry (SR) — in-kernel rows

| ID | Invariant | Carrier |
|---|---|---|
| SR-1 | Registry frozen at startup, no runtime registration | excluded: build-time composition mechanics — which flavors are linked is a build artifact, not kernel ontology (Composition.lean deleted 2026-06-28, D16) |
| SR-2 | Every memory payload schema-typed | structure field `Memory.schema : SchemaRef` (accessor totality — every row carries an opaque schema tag). "Registered in the active registry" is engine admission, not a kernel rule (D16) |
| SR-8 | Schema ids flavor-qualified | excluded: namespacing is engine id-minting (collision-freedom = "the engine mints distinct ids"), not kernel ontology (D16) |
| SR-11/16 | F/A/P may carry optional free text; sidecars may carry opaque typed payload | structure field `Memory.text : Option Text`; sidecar semantics deferred |
| SR-13 | Fact identity ≠ payload hash; UUIDv7 | structure field `Memory.id` + table/store invariant `MemoryIdUnique` + ST-22 comment |
| SR-14/44 | Fact has no supersedes | THEOREM `facts_never_supersede` |
| SR-24 | relation_class closed, not flavor-extensible | inductive `RelationClass` (note: doc 03 does NOT enumerate classes — doc 02's five are authoritative; no contradiction, verified 2026-06-11 against doc 03 §EdgePayload verbatim) |
| SR-25 | Edges immutable v1 | `Immutable Edge` |
| SR-30..33 | special_category per schema, author-declared | excluded: GDPR controller/engine concern (`schema_special_category` cut 2026-06-28, D16) — the kernel never reasons over special-category |
| SR-43 | Stateful Fact: each observation a new Fact | `Immutable`-stance + ST-14 row |
| SR-46 | Tombstone is a Fact (deletion is observed state) | comment Identity.lean / excluded detail: stateful-schema mechanics |
| SR-49 | Memory row stores schema id+version+kind | accessors `memory_schema`, `memory_kind` |
| SR-56/57 | Layer/kind change in migration forbidden | excluded: migration mechanics; spirit = kind is fixed per memory (accessor totality, immutability) |

## 08 — Core & Flavors (CF) — in-kernel rows

| ID | Invariant | Carrier |
|---|---|---|
| CF-G | Payload opacity — the domainless boundary | structural ABSENCE: `SchemaRef` (Identity) is an opaque per-row tag with NO accessor at all (no resolution, no payload, no capabilities); the kernel sees a row is schema-typed and nothing more |
| CF-* (compliance) | A flavor must comply with the basic rules | THEOREMS in `Causa.Flavor` — a concrete flavor's rows discharge the universal invariants (`fact_is_fact`, `abstraction_grounded`, `published_readable`/`published_read_only`, `wipeable_when_abandoned`) via only pre-existing theorems; the rules quantify over every row, so compliance is derived, never axiomatized |
| CF-OPEN | Substrate is OPEN — any app integrates as a flavor with zero kernel change and no new axiom | `Causa.Flavor` constructive witness: a flavor's vocabulary is inhabitants of existing types (`SchemaRef`/`RelationId`) taken as PARAMETERS, not axioms; `#print axioms` on the flavor theorems names only pre-existing kernel axioms — never one named `flavor`. That machine-checked absence IS the openness (D16/D18) |
| CF-43..46 | Goal entity core-owned; flavor owns payload/tools | structural: Goal lives in the kernel; payloads opaque |
| CF-60 | Cross-flavor reads obey Owner/access surface | Owner/read authorization (Causa.Authorization); no materialized personality read-scope after D4 |

## 07 — Storage (ST) — in-kernel rows

| ID | Invariant | Carrier |
|---|---|---|
| ST-1..4 | Fresh ids; immutable identity; supersession = new row | `Memory.id` + `MemoryIdUnique`, `Goal.id` + `GoalIdUnique`, classes `Immutable`/`AppendOnly`; memory supersession is `memorySupersedes` over Supersession-class edges; Goal supersession stores prior `GoalId` |
| ST-5 | Edges insert-only | `Immutable Edge`, `AppendOnly Edge` |
| ST-6 | source-ingest dedup key deterministic; duplicate = replay | excluded: source/flavor ingest metadata after D1; no core `EventId` entity |
| ST-7/8 | CitedObject/CitationMapping ids, insert-only, one mapping per Fact | structural ids + scoped defs `CitedObjectIdUnique`/`CitationMappingIdUnique`/`CitationMappingUniqueByFact`, `Immutable`/`AppendOnly` instances + theorem `citation_unique_per_fact` |
| ST-9 | Owner identity columns (principal kind + id) | `Owner := Group` (`Set User`) — the kernel models owner as group membership over the atom `User`; a personal owner is `Owner.ofUser u`. The (kind+id) column shape is engine storage. org has no kernel face — decisions `2026-06-11-org-out-of-kernel.md`, S0 collapse, owner realign 2026-06-28 |
| ST-10 | Edge ownership: source-owned rows; only Supersession forbids cross-owner target | row-validity predicates `EdgeSourceOwned`, `EdgeSupersessionIntraOwner`; non-Supersession cross-owner targets are allowed by source-owned Edge policy |
| ST-11 | INSERT-only cognitive lifecycle | class `AppendOnly` + instances |
| ST-13 | Only compliance erasure deletes | Compliance.lean: def `abandoned` is the SOLE delete trigger (owning group empty) + THEOREMs `drop_personal_abandoned`, `source_abandoned_cascades_to_edge`, `world_never_abandoned` — realign 2026-06-28 (was axioms `erased`/`erasure_removes_cognitive`) |
| ST-14 | Stateful current-state = head query, never replacement | comment + SR-43 row |
| ST-15..17 | Vector-store independence (targets F/A/P AND Goals) | structural ABSENCE: no kernel `Embedding` entity, no `Memory → Embedding` accessor — embeddings are engine-side (`EmbeddingTarget`/`Embedding`/`embedding_target` retired 2026-06-28; the invariant was always the absence, never the declared type) |
| ST-22/23 | Content hash/dedup key not Fact identity; collision semantics | `Memory.id` remains Fact identity; source/flavor ingest dedup key excluded after D1 |
| ST-26 | Supersession logical; current state = query | defs `memorySupersedes`/`memoryIsHead`, table-scoped `goalIsHead`/`activeGoals` pattern + comment |

## 11 — Citations (CI)

| ID | Invariant | Carrier |
|---|---|---|
| CI-1 | Fact-only citation (citation ⇒ Fact; OPTIONAL on Facts since 2026-06-13) | structural `CitationMapping.fact : Fact`; THEOREMs `citation_fact_is_fact`, `citation_implies_fact` over table-scoped choice-def `memory_citation` |
| CI-2 | At most one mapping per Fact in a valid mapping table; target is Fact; no orphans | validity predicate `CitationMappingUniqueByFact`; THEOREMs `citation_points_back`, `citation_points_to_row`, `citation_reverse_total`, `citation_unique_per_fact` |
| CI-3 | A/P cite transitively via provenance | comment + CN-6 axioms |
| CI-7/8 | Owner scoping; Fact owner = object owner | structural field `CitationMapping.owner_match`; THEOREM `citation_owner_match` |
| CI-9 | One object ↔ N mappings | structural absence of object-side restriction |
| CI-12/13 | Edges do not cite | structural absence of citation accessor on Edge |
| CI-14 | Operator provenance = row metadata, not citation | comment Operators.lean |
| CI-4/5/6/10/11 | content-hash idempotency mechanics | excluded: engine (hash invisible below opacity boundary) |
| CI-15/16 | S3 bytes, presigned URLs | excluded: engine storage |

## 13 — Compliance (CO)

| ID | Invariant | Carrier |
|---|---|---|
| CO-1 | Cognitive lifecycle append-only | classes + instances (ST-11 row) |
| CO-7 / ST-13 | Erasure = ABANDONMENT (reference count zero): an entity whose owning group has no members is wipeable; a user dropping abandons their personal group | def `abandoned` (`∀ u, o u = none`) + THEOREM `drop_personal_abandoned` over `Group.drop`; edge cascade THEOREM `source_abandoned_cascades_to_edge`; retention boundary THEOREM `world_never_abandoned`. Realign 2026-06-28 — replaces axioms `erased`/`erasure_removes_cognitive` + THEOREM `erasure_removes_edges`; Compliance.lean 6 axioms → 0 |
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
| GO-16 | Operator-authored Goals do not carry materialized authoring personality | structural absence: no `goal_authoring_personality`; evidence carried by A→Goal edges |
| CI-17 | Cited objects / mappings schema-registered | structure fields `cited_object_schema`/`citation_mapping_schema : SchemaRef` (schema-typed); registration is engine admission (D16) |
| ST-EdgeId | source-ingest edges content-hash id vs UUIDv7 (AGENTS.md inv. 17) | inductive `EdgeId` sum + row-validity predicate `EdgeIdAuthorshipValid` + projection THEOREM `edge_id_authorship_split` |

## Principle surface map

| Principle | Named surface prop | Kernel carrier |
|---|---|---|
| P1 | `principle_1_facts_below_perspective` | `MemoryKind.layer` theorem: Fact layer below Perspective. |
| P2 | `principle_2_operator_goals_carry_evidence` | `EdgeOperatorShapeValid` + `operatorEdgeShape .OperatorAtoGoal`; WEAKENED to operator-derived Goals only, with goal measurement/justification left to a decider. |
| P3 | `principle_3_operators_never_output_facts`; `principle_3b_goal_close_is_an_act`; `principle_3c_causal_closure_is_perspectival` | `operator_memory_output_not_fact` over `EdgeOperatorShapeValid`; `terminal_goal_closes_with_fact` + `goal_close_fact`; `causal_goal_edge_perspectival` over `EdgeCoreValid`. |
| P4 | `principle_4_facts_connect_non_interpretively` | `legalClasses .Fact .Fact`. |
| P5 | `principle_5_memories_grounded_in_facts` | `grounding_wf` (now a THEOREM from `derivation_created_at_strict` — acyclicity follows from the strict arrow of time, no longer an axiom) + `derived_has_provenance` → `memory_grounds_in_facts`; A→A provenance via the matrix cell + `operatorEdgeShape .OperatorAtoA`. |
| P6 | `principle_6a_derivation_provenance_strictly_upward`; `principle_6b_personality_read_scope_removed` | `edge_layer_rule` over `EdgeCoreValid`; structural absence of `read_scope`/`personality_may_read`; wake context deferred. |
| P7 | `principle_7_personality_is_not_entity` | structural absence: no personality row/type/instance; no Personality module. |

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
