# Kernel Coverage Matrix — docs → axioms

Every invariant extracted from the source docs (inventories of 2026-06-11) maps
to exactly one row: a kernel construct, a structural carrier ("the shape IS the
rule"), or an explicit exclusion with reason. Axiom names are in
`docs/lean/Causa/*.lean`; namespace `Causa`.

Carrier kinds: `axiom` | `def` | `inductive`/`structural` (the type shape
carries the rule) | `comment` (recorded as design commentary at the named
location) | `excluded` (engine/app concern, not ontology).

Baseline axiom count after transcription: **134** (per-file: Composition 23,
Memory 22, Identity 19, Edges 16, Goals 13, Operators 11, Citations 11,
Owner 9, Compliance 7, Prelude 3). The minimization pass updates this section
with before/after counts.

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
| ES-2 | Visibility rule (group membership) | def `visible` (`requester ∈ o`) + theorem `visible_personal` |
| ES-3 | org never enters access or identity | structural: org absent from the kernel entirely (`Owner := Group`, a `Set User`) — decisions `2026-06-11-org-out-of-kernel.md`, owner realign 2026-06-28 |
| ES-4 | source-ingest dedup key deterministic over source/owner/payload | excluded: source/flavor ingest metadata after D1; no core `EventId` entity |
| ES-5 | Batch id unique within (source, owner) | excluded: per-scope engine validation with no kernel-observable face; F→A gate carries owner dimension (`ftoa_batch_exclusive`); wake context dimension deferred after D4 |
| ES-6 | Facts are typed observations, not operator derivations | `Fact` subtype + `operator_memory_output_not_fact`; source trace metadata is flavor/ingest-side after D1 |
| ES-7 | 1:1 source-ingest receipt→Fact materialization | excluded: source/flavor ingest implementation; core sees only typed Fact rows after D1 |
| ES-8 | Source must not abstract/interpret/cross-join/relevance-filter/persist | excluded: source/flavor ingest contract; kernel carries the consequence via `operator_memory_output_not_fact` + no downward operator writes |
| ES-9 | Compliance metadata: every source declares 4 fields; Facts inherit | excluded: engine registration totality (CO-39..45 rows); kernel keeps SourceId opaque |
| ES-10 | Idempotency keys content-derived/opaque, never natural-person identifiers | comment Compliance.lean suppression docstring (CO-20) |
| ES-11 | Facts' typing frozen at insert; engine does not migrate Facts across schema versions | `Immutable Memory`-stance via `AppendOnly Memory` + accessor totality (`memory_schema` fixed per row); migration mechanics excluded (SR-50..55 exclusion block) |
| ES-12 | No `Principal.Org` variant; org-wide = `<org>-everyone` group | structural: no `Principal` sum at all — `Owner := Group` (`Set User`); a User is the singleton group `Owner.ofUser u`, org-wide is an ordinary shared (non-singleton) Group. Realign 2026-06-28 |
| ES-13 | Per-memory ACL (AccessGrant) is v2+, not v1 | structural absence + Owner.lean header comment |
| ES-14 | Push vs pull is source-side implementation detail | excluded: engine |
| ES-15 | Bootstrap/founding-goal is flavor onboarding | excluded: app-layer |
| AUTH-1 | Owner-space grant predicate | `Authorization.lean` `MemoryAction`, `owner_space_grant`, `may_memory_action` |
| AUTH-2 | Grants do not replace Owner visibility (all principal subjects) | def `principal_can_access` + axiom `owner_space_grant_owner_visible` |
| AUTH-3 | Per-memory ACL absent in v1 | excluded: doc 01 keeps `AccessGrant` as v2+ extension layered above Owner |

## 02 — Memory (ME)

| ID | Invariant | Carrier |
|---|---|---|
| ME-1 | Fact is Memory with kind `.Fact` | subtype `Fact := { m : Memory // memory_kind m = .Fact }` + theorem `fact_memory_kind` |
| ME-2 | Fact owner is the memory row owner | structural: `Fact.memory` projects to `Memory.owner`; source/event owner inheritance moved out of core by D1 |
| ME-3 | Optional free text is a Memory field for F/A/P; no kind-based text axiom | structure field `Memory.text : Option Text` + accessor `memory_text` |
| ME-4 | Facts never supersede / never superseded | THEOREM `facts_never_supersede` (Edges.lean, from Supersession-class edge + matrix) |
| ME-5a | Supersession same kind | THEOREM `supersession_same_kind` (Edges.lean) |
| ME-5b | Supersession same owner | THEOREM `supersession_same_owner` (Edges.lean, via `supersession_intra_owner`) |
| ME-6 | Personality is not a materialized Memory author/owner slot | structural absence: no `PersonalityInstance`, no `personality_owner`, no `memory_authoring_personality`; D4 comment Memory.lean |
| ME-7 | Facts below Perspectives; no personality read-scope matrix | theorem `principle_1_facts_below_perspective`; structural absence of `read_scope`/`personality_may_read`; wake read context deferred |
| ME-8 | Materialized personality matrix removed | structural absence of `read_scope` and matrix-version state; wake context/read semantics deferred after D4 |
| ME-9 | Edge scope single-owner | axiom `edge_scope_single_owner` |
| ME-10 | ℓ(source) ≥ ℓ(target) for memory edges | THEOREM `edge_layer_rule` (from the matrix's empty upward cells) |
| ME-11 | Class-legality matrix (9 cells) | def `legalClasses` + THEOREM `edge_class_legal` (from mask axioms `edge_respects_mask` + `descriptor_masks_tighten_only`) |
| ME-12 | Supersession same endpoint shape (incl. Goal→Goal) | axiom `supersession_same_endpoint_shape` |
| ME-13 | Edges immutable v1 | `Edge` structure + instances `Immutable Edge`, `AppendOnly Edge` |
| ME-14 | Descriptor masks tighten, never relax | axioms `relation_endpoint_admitted`, `descriptor_masks_tighten_only`, `edge_respects_mask` |
| ME-15 | Causal chain is a query, not an entity; materialized = cache only | structural absence + Edges.lean header |
| ME-16 | Memory id is identity | structure field `Memory.id` + table/store invariant `MemoryIdUnique` |
| ME-17 | Personality is emergent from Perspective/wake context, not a stored instance | structural absence in Memory.lean/Principles.lean; no Personality module; wake entries deferred |
| ME-18 | Cross-context supersession policy | excluded: wake/Perspective context semantics deferred after D4; no personality instance axis in kernel |
| ME-19 | Relation registry: unregistered relations invalid | `entities_use_registered_vocabulary` (Composition) |
| ME-20 | Core relations table (derived-from/supersedes/inspires/authored) | excluded: vocabulary content, not law — flavors/core register ids; classes pinned by `RelationClass` |

## 04 — Consolidation (CN)

| ID | Invariant | Carrier |
|---|---|---|
| CN-1 | F→A edge shape | def `operatorEdgeShape` + axiom `operator_edges_shaped`; full shape THEOREM `ftoa_edge_shape` |
| CN-2 | A→P edge shape | same merged carrier; full shape THEOREM `atop_edge_shape` |
| CN-3 | A→Goal evidence shape (Structural class) | `operatorEdgeShape` OperatorAtoGoal arm + `operator_edges_shaped` |
| CN-4 | frame/PerspectiveLink shape | `operatorEdgeShape` PerspectiveLink arm + `operator_edges_shaped` |
| CN-5 | No downward writes | theorem `operator_memory_output_not_fact` + `operatorEdgeShape` + ME-1 Fact subtype |
| CN-6 | Derived memories have provenance | axiom `derived_has_provenance` (merged); per-kind shapes THEOREMs `abstraction_has_provenance`, `perspective_has_provenance` |
| CN-7 | Cross-domain join is typed Abstraction | comment (shape carried by CN-6 + U-2 matrix) |
| CN-8 | F→A batch-gate exclusivity per (owner, batch, input contract, operator, output schema) | axiom `ftoa_batch_exclusive`; wake context dimension deferred after D4, no personality dim |
| CN-9 | Atomic invocation (all-or-nothing outputs) | excluded: storage-layer transaction contract (same stance as WH event/projection atomicity) |
| CN-10 | Retry/changed-prompt = new derivation, never mutation | `AppendOnly Memory` + comment Operators.lean |
| CN-11 | Wake dispatcher loop, cursors, depth bound, runtime tables | excluded: engine runtime |
| CN-12 | Prompt locality (core ships no domain prompts) | excluded: engine/flavor split mechanics; spirit carried by CF-A |

## 06 — Goals (GO)

| ID | Invariant | Carrier |
|---|---|---|
| GO-1 | Supersession same owner | axiom `goal_supersession_constraints` (merged); projection THEOREM `goal_supersession_same_owner` |
| GO-2 | Valid lifecycle transition | def `goalTransitionAdmitted` + merged axiom; projection THEOREM `goal_supersession_admitted` |
| GO-3 | Goal DAG acyclic | inductive `goalAncestor` + axiom `goal_parents_acyclic` |
| GO-4 | Parents same owner | axiom `goal_parents_same_owner` |
| GO-5 | Every transition new row; no in-place mutation | `AppendOnly Goal` + `Supersedable Goal` |
| GO-6 | Goal is not Memory | structural: distinct Types |
| GO-7 | Self is a query, never an entity/cache | structural absence + Goals.lean header |
| GO-8 | Active set definition (heads, state=Active) | defs `goalIsHead`, `activeGoals` |
| GO-9 | Goal id is identity | axiom `goal_id_injective` |
| GO-10 | Authorship vocabulary | inductive `GoalAuthorship` |
| GO-11 | GoalWrite protocol (request_id idempotency, conflict detection, stream visibility) | excluded: protocol surface (doc 14 out of scope per spec) |
| GO-12 | Assignment = core/inspires edge Goal→Self-Perspective; instance-scoped active_goals query | shape via descriptor masks (ME-14); traversal query engine-level per decision `2026-06-11-active-goals-two-queries.md` |
| GO-13 | Goal-scoped wake policy; planner-first | excluded: engine runtime |
| GO-14 | Cross-owner assignment/evidence rejected | axioms `edge_scope_single_owner` + `goal_parents_same_owner` |

## 03 — Schema Registry (SR) — in-kernel rows

| ID | Invariant | Carrier |
|---|---|---|
| SR-1 | Registry frozen at startup, no runtime registration | axiom `registry_composition` (the law); THEOREM `registry_determined` |
| SR-2 | Every memory payload schema-typed | accessor totality + `entities_use_registered_vocabulary` (merged) |
| SR-8 | Schema ids flavor-qualified | axiom `schema_namespace` + THEOREM `registry_namespace_discipline` |
| SR-11/16 | F/A/P may carry optional free text; sidecars may carry opaque typed payload | structure field `Memory.text : Option Text`; sidecar semantics deferred |
| SR-13 | Fact identity ≠ payload hash; UUIDv7 | structure field `Memory.id` + table/store invariant `MemoryIdUnique` + ST-22 comment |
| SR-14/44 | Fact has no supersedes | THEOREM `facts_never_supersede` |
| SR-24 | relation_class closed, not flavor-extensible | inductive `RelationClass` (note: doc 03 does NOT enumerate classes — doc 02's five are authoritative; no contradiction, verified 2026-06-11 against doc 03 §EdgePayload verbatim) |
| SR-25 | Edges immutable v1 | `Immutable Edge` |
| SR-30..33 | special_category per schema, author-declared | axiom `schema_special_category : SchemaId → Bool` (domain shape) |
| SR-43 | Stateful Fact: each observation a new Fact | `Immutable`-stance + ST-14 row |
| SR-46 | Tombstone is a Fact (deletion is observed state) | comment Identity.lean / excluded detail: stateful-schema mechanics |
| SR-49 | Memory row stores schema id+version+kind | accessors `memory_schema`, `memory_kind` |
| SR-56/57 | Layer/kind change in migration forbidden | excluded: migration mechanics; spirit = kind is fixed per memory (accessor totality, immutability) |

**SR exclusions (engine/SQL/Rust mechanics):** SR-3..7, SR-9, SR-10, SR-12,
SR-15, SR-17..23, SR-26..29, SR-34..42, SR-45, SR-47, SR-48, SR-50..55,
SR-58..72 — sidecar tables, validators, renderers, migration discipline,
opaque-schema rules, natural-key queries. Reason: typed-payload mechanics are
engine contracts beneath the opacity boundary (CF-G); the kernel deliberately
cannot see payloads.

## 08 — Core & Flavors (CF) — in-kernel rows

| ID | Invariant | Carrier |
|---|---|---|
| CF-1 | Core owns substrate; flavors contribute build-time vocabulary | axioms `core_vocabulary`/`flavor_schemas`/`registry_composition`; THEOREM `core_always_present` (CF-A) |
| CF-2/3 | Build-time choice; no runtime registration tier | `registry_composition`; THEOREM `registry_determined` (CF-D) |
| CF-20..24 | Namespace prefix discipline | axiom `contributions_namespaced`; THEOREM `registry_namespace_discipline` (CF-B) |
| CF-49/50 essence | No cross-flavor collision | structural (identity carries namespace, CF-C comment) |
| CF-54/55 | Composite binary = build artifact, not plugin host | axiom `active_registry` (one ambient registry) + comment |
| CF-56 | No feature flags; linkage + register() | excluded: Rust mechanics; spirit = CF-D |
| CF-57/58 | Registry ownership per flavor; prefixes kept in composite | CF-B + CF-C |
| CF-60 | Cross-flavor reads obey Owner/access surface; personality read-scope absent | Owner/read authorization lives outside materialized personality; no `personality_may_read` after D4 |
| CF-61 | Cross-flavor edges use registered descriptors | THEOREM `edges_use_registered_relations` (projection of `entities_use_registered_vocabulary`) |
| CF-61b | Relation endpoint descriptors may require opaque schema capability tags; admitted endpoints declare the required side tags | axiom `relation_endpoint_required_tags_valid` |
| CF-43..46 | Goal entity core-owned; flavor owns payload/tools | structural: Goal axioms live in kernel; payloads opaque |

**CF exclusions:** CF-4..19, CF-25..42, CF-47..53, CF-59, CF-62 — macro
surface, Cargo metadata, freeze-guard panics, tool packs, and wake-entry
validation. Reason: Rust composition mechanics; their ontology content is
CF-A/B/C/D above.

## 07 — Storage (ST) — in-kernel rows

| ID | Invariant | Carrier |
|---|---|---|
| ST-1..4 | Fresh ids; immutable identity; supersession = new row | `Memory.id` + `MemoryIdUnique`, `goal_id_injective`, classes `Immutable`/`AppendOnly`; memory supersession is `memorySupersedes` over Supersession-class edges |
| ST-5 | Edges insert-only | `Immutable Edge`, `AppendOnly Edge` |
| ST-6 | source-ingest dedup key deterministic; duplicate = replay | excluded: source/flavor ingest metadata after D1; no core `EventId` entity |
| ST-7/8 | CitedObject/CitationMapping ids, insert-only, one mapping per Fact | structural ids + scoped defs `CitedObjectIdUnique`/`CitationMappingIdUnique`, `Immutable`/`AppendOnly` instances + theorem `citation_unique_per_fact` |
| ST-9 | Owner identity columns (principal kind + id) | `Owner := Group` (`Set User`) — the kernel models owner as group membership over the atom `User`; a personal owner is `Owner.ofUser u`. The (kind+id) column shape is engine storage. org has no kernel face — decisions `2026-06-11-org-out-of-kernel.md`, S0 collapse, owner realign 2026-06-28 |
| ST-10 | Cross-owner edges/evidence rejected | axiom `edge_scope_single_owner` |
| ST-11 | INSERT-only cognitive lifecycle | class `AppendOnly` + instances |
| ST-13 | Only compliance erasure deletes | Compliance.lean (`erased`, `erasure_removes_cognitive`) |
| ST-14 | Stateful current-state = head query, never replacement | comment + SR-43 row |
| ST-15..17 | Vector-store independence (targets F/A/P AND Goals) | `EmbeddingTarget` sum + `embedding_target` + `Immutable Embedding`; absence of entity→Embedding accessor |
| ST-22/23 | Content hash/dedup key not Fact identity; collision semantics | `Memory.id` remains Fact identity; source/flavor ingest dedup key excluded after D1 |
| ST-26 | Supersession logical; current state = query | defs `memorySupersedes`/`memoryIsHead`, `goalIsHead`/`activeGoals` pattern + comment |

**ST exclusions:** ST-12 (sidecar migration), ST-18..21 (core/flavor SQL
ownership — spirit in CF-A), ST-24/25 (partitioning physical). Reason:
storage-layout mechanics.

## 11 — Citations (CI)

| ID | Invariant | Carrier |
|---|---|---|
| CI-1 | Fact-only citation (citation ⇒ Fact; OPTIONAL on Facts since 2026-06-13) | structural `CitationMapping.fact : Fact`; THEOREMs `citation_fact_is_fact`, `citation_implies_fact` over the choice-def `memory_citation` |
| CI-2 | Exactly one mapping per Fact; target is Fact; no orphans | axiom `citation_fact_injective`; THEOREMs `citation_points_back`, `citation_reverse_total`, `citation_unique_per_fact` |
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
| CO-2/6 | Compliance out-of-band, never a Memory mutation | inductive `ComplianceOp` + header comment |
| CO-3 | Scope = one Owner or Owner-scoped source object | constructor shapes of `ComplianceOp` |
| CO-4/5 | Admin-authored; operator visibility diminished | excluded: authorization surface (engine/protocol) |
| CO-7 | delete_owner removes cognitive, retains suppression+audit | axioms `erased`, `erasure_removes_cognitive` (Memory+Goal); THEOREM `erasure_removes_edges`; suppression survival structural |
| CO-8 | delete_source_scope | constructor `DeleteSourceScope` (semantics engine-resolved per flavor — comment) |
| CO-9/10 | Pause/resume semantics | axiom `paused` + comment (dispatch gate engine-enforced) |
| CO-12/13/14 | Outcomes incl. refusal-is-valid | inductive `ComplianceOutcome` |
| CO-15/16/20 | Suppression retains opaque key only | accessor shape `suppression_key : SuppressionEntry → SuppressionKey` |
| CO-17/18 | Suppression blocks re-ingest | axiom `suppression_blocks_reingest` |
| CO-19/29 | Suppression/audit survive erasure indefinitely | structural (unconditional quantification; Compliance.lean comment) |
| CO-11, CO-21..28, CO-30..58 | Export, audit content, side effects, vocabulary fields, owner policy, GDPR mappings | excluded: controller/engine obligations and legal commentary; ES-9/ES-10 rows carry the kernel-relevant faces |

### r1 additions (codex review, 2026-06-11)

| ID | Invariant | Carrier |
|---|---|---|
| GO-2b | Stale prior cannot be lifecycle head | axiom `goal_supersession_prior_is_head` |
| GO-15 | Goal title/text core retrieval text | axioms `goal_title`, `goal_text` |
| GO-16 | Operator-authored Goals do not carry materialized authoring personality | structural absence: no `goal_authoring_personality`; evidence carried by A→Goal edges |
| CI-17 | Cited objects / mappings schema-registered | `entities_use_registered_vocabulary` arms 4+5 |
| ST-EdgeId | source-ingest edges content-hash id vs UUIDv7 (AGENTS.md inv. 17) | inductive `EdgeId` sum + axiom `edge_id_authorship_split` (Edges.lean) — exclusion reversed after both reviewers + AGENTS.md elevated it |

## Minimization log

**Pass 1 (2026-06-11, workflow: 9 analyzers + adversarial verify, 25
proposals, 23 verified):** post-r1 count 151 axioms → **132 axioms + 23
theorems**, lake-build green. Of the 132, ~60 are primitive declarations
(opaque Types + accessors); the rest are invariants.

Implemented reductions:
- Owner → constructive subtype def (`Owner`, `owner_principal`, `owner_org`
  defs); ES-1 `owner_org_denormalized` PROVED. −4 axioms.
  *(Superseded 2026-06-11 morning review: org demoted to engine entirely,
  `Owner := Principal`, `OrgId`/`group_org` removed, ES-1 dissolves —
  130 axioms; decision `2026-06-11-org-out-of-kernel.md`.)*
  *(Further realign 2026-06-28: the `Principal` sum, `group_members`, and
  `owner_principal` are gone. One atom `User`; `Group := Set User`;
  `Owner := Group`; a User is the singleton group `Owner.ofUser u`.
  Owner.lean 3→1 axiom; in Authorization `world` becomes the def
  `fun _ => True` and the dead `world_is_group` is dropped (−2 axioms).)*
- Edges: descriptor masks are now the primitive layer; `edge_class_legal`
  (ME-11) and `edge_layer_rule` (ME-10) PROVED from them.
- Memory supersession: row pointer/accessor removed; `memorySupersedes` is a
  definition over Supersession-class edges. ME-4, ME-5a, ME-5b PROVED from
  the matrix + supersession owner scope.
- Operators: CN-1..4 merged into `operatorEdgeShape` def + ONE axiom; CN-1/2
  full shapes (target kinds) PROVED via `provenance_pins_target`. CN-6 merged
  to `derived_has_provenance`; per-kind shapes PROVED. Net −5.
- Citations: CitedObject/CitationMapping are structural thin evidence
  anchors; blob storage/hash/range payload stays flavor/engine-side.
  Fact-side pointer `memory_citation` is a choice-based DEF; primitive
  uniqueness is `citation_fact_injective`; CI-1 (citation ⇒ Fact;
  `fact_has_citation` retired 2026-06-13 — citations optional on Facts),
  CI-2a, CI-2c, and owner match are PROVED.
- Goals: GO-1+GO-2 merged into `goal_supersession_constraints`; projections
  PROVED. −1.
- Compliance: `suppression_owner` REMOVED (doc 13 retains the opaque key
  only — entry carries no Owner; ES-4 makes key-matching scope-matching);
  erasure Edge conjunct PROVED (`erasure_removes_edges`). −2.
- Composition: registry restructured around the composition law
  (`registry_composition` + `flavor_schemas`/`flavor_relations` +
  `contributions_namespaced`); `core_always_present`,
  `registry_namespace_discipline`, `registry_determined` (pointwise) PROVED.
  Six registration axioms merged into `entities_use_registered_vocabulary`.
  Net −5, and the over-strong cross-binary determination axiom is gone.

Over-strength corrections (doc-fidelity, flagged in decisions/):
- `batch_unique_within_source_owner` REMOVED (global injectivity exceeded the
  docs' scoped uniqueness); `ftoa_batch_exclusive` owner-conditioned →
  `decisions/2026-06-11-batch-id-scope.md`.
- `read_scope_diagonal` later removed with materialized `PersonalityInstance`; wake context/read semantics deferred.
- `goal_parents_same_owner` KEPT, re-cited to doc 04 §Isolation →
  `decisions/2026-06-11-goal-parents-owner-scope.md`.

Additions forced by review (both reviewers + AGENTS.md invariant 17):
- `EdgeId` is now the ContentHash/UUIDv7 SUM with `edge_id_authorship_split`
  (Edges.lean) — supersedes the r1 ST-EdgeId exclusion row.

Rejected by adversarial verify: 2 proposals (one stale, one inverse of an
already-landed r1 fix).

## Principle surface map

| Principle | Named surface prop | Kernel carrier |
|---|---|---|
| P1 | `principle_1_facts_below_perspective` | `MemoryKind.layer` theorem: Fact layer below Perspective. |
| P2 | `principle_2_operator_goals_carry_evidence` | `operator_edges_shaped` + `operatorEdgeShape .OperatorAtoGoal`; WEAKENED to operator-derived Goals only, with goal measurement/justification left to a decider. |
| P3 | `principle_3_operators_never_output_facts`; `principle_3b_goal_close_is_an_act`; `principle_3c_causal_closure_is_perspectival` | `operator_memory_output_not_fact`; `terminal_goal_closes_with_fact` + `goal_close_fact`; `causal_goal_edge_perspectival`. |
| P4 | `principle_4_facts_connect_non_interpretively` | `legalClasses .Fact .Fact`. |
| P5 | `principle_5_memories_grounded_in_facts` | `grounding_wf` + `derived_has_provenance` → `memory_grounds_in_facts`; A→A provenance via the matrix cell + `operatorEdgeShape .OperatorAtoA`; temporal companion `derivation_created_at_monotone`. |
| P6 | `principle_6a_derivation_provenance_strictly_upward`; `principle_6b_personality_read_scope_removed` | `edge_layer_rule`; structural absence of `read_scope`/`personality_may_read`; wake context deferred. |
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
