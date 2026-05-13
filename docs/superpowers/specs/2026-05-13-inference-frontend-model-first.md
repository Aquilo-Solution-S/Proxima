# Inference frontend — model-first redesign

**Status:** design  
**Scope:** `Settings > Models` view in `packages/frontend-core`  
**Out of scope:** engine API changes for inference target storage; secret-management UI; wake-trigger override UI (lives in `Personalities`)

## Why

Today's `Settings > Models` page is tier-first: three preset tier cards (`fast`/`standard`/`deep`), each carrying its own config form, plus a hidden `<details>` accordion for custom targets and tier bindings. The current page has six concrete usability problems:

1. Layout wastes horizontal space — content sits in the left half of a wide window because the tier row uses a fixed 90px gutter and `flex-wrap` makes provider/model strings break across lines.
2. Tier semantics are invisible — newcomers see `fast`/`standard`/`deep` with no description of when each fires.
3. API key health is invisible — the card shows the env var *name* but never whether it's *set* in the Tauri process.
4. No connection test — broken targets only surface when a wake fails.
5. `<details>` accordion buries custom targets and tier bindings under "advanced", so there's no single map of "what target is each tier using right now".
6. Standard and Deep collide visually when configs match — both rows show the same `target_ref` tag because `persistTier` reuses an existing target via `sameConfig`, but the UI gives no hint that one tier is aliased onto another.

Tier-first also fights the engine's data model. The engine treats `inference_targets` as the entity table; tier bindings and `personality_wake_entries.inference_target_ref` are both *pointers* to a target. The schema is already wired for per-wake-entry overrides (`inference_target_ref` exists on `personality_wake_entries` as of `20260507000030`), and the next step in this surface is exposing that override in the Personalities view. A model-first inference settings page is the right foundation: one row per model, with tier assignment as a property of the row, and a hook ready for "used by N wake entries".

## Design

### Information architecture

`Settings > Models` becomes a two-sub-tab view:

- **Chat** — registered chat inference targets and their tier assignments (this spec)
- **Embedding** — existing embedding section, moved verbatim into the second sub-tab

The current `index.tsx` stacks Inference and Embedding as peer sections; the redesign uses a tab strip inside the Models pane so each surface has full width.

### Chat sub-tab layout

```
SETTINGS > MODELS                                       [ Chat ] [ Embedding ]

CURRENT TIER ASSIGNMENT
  fast → my-mistral-fast      standard → openai-medium      deep → openai-deep
  (low-stakes wakes)          (most wakes)                  (deep-think wakes)

REGISTERED MODELS                                          [ + Register model ]

┌──────────────────────────────────────────────────────────────────────────────┐
│ ref              provider · model                       key      F  S  D  ⋯ │
├──────────────────────────────────────────────────────────────────────────────┤
│ my-mistral-fast  Mistral Chat                                                 │
│                  mistral-medium-latest                  ● set    ●  ○  ○  ⋯ │
│                  wake overrides: 0 · assign in Personalities (coming soon)    │
├──────────────────────────────────────────────────────────────────────────────┤
│ openai-medium    OpenAI Responses · medium reasoning                          │
│                  gpt-5.3-codex-spark                    ● set    ○  ●  ○  ⋯ │
│                  wake overrides: 0 · assign in Personalities (coming soon)    │
├──────────────────────────────────────────────────────────────────────────────┤
│ openai-deep      OpenAI Responses · high reasoning                            │
│                  gpt-5.5                                ● set    ○  ○  ●  ⋯ │
│                  wake overrides: 0 · assign in Personalities (coming soon)    │
└──────────────────────────────────────────────────────────────────────────────┘
```

**Header strip** — the "current tier assignment" block resolves each tier to its bound `target_ref` and shows a one-line description per tier. This is the only place tier descriptions appear; the model table itself stays terse.

**Model table** — one row per registered target. Columns:
- `ref` — `target_ref` (mono, chip-styled)
- `provider · model` — kind label, model id, and (when applicable) reasoning effort or temperature/max-tokens caveats
- `key` — API key health pill (`● set` green / `○ missing` red), computed from `inference_env_status`
- `F` / `S` / `D` — tier radios (filled = bound to this row)
- `⋯` — overflow menu: Test connection, Edit, Remove

**Tier radio interaction** — clicking an empty radio in column F/S/D calls `bindInferenceTier({ tier, target_ref })` and refetches. The filled radio in the previously-bound row updates accordingly. Clicking an already-filled radio is a no-op (there is no "unbind"; the tier can only be reassigned to another row). The engine's tier-binding table is constrained to one row per tier; the UI relies on that invariant.

**Wake overrides subline** — rendered disabled with count `0` for v1. The subline exists so the surface is ready when per-wake-entry override UI lands. Tooltip: "Coming soon — assign models per wake trigger from Personalities."

### Register model flow

Replace the `<details>Custom target (advanced)</details>` accordion with a `[ + Register model ]` button that opens a modal containing the existing `TargetDraftEditor`. The modal's title is "Register model" (not "Register custom target") — every model goes through this same flow.

The form pre-fills provider-fact defaults when the user picks a Kind:
- `mistral_chat` → base_url `https://api.mistral.ai`, api_key_env `MISTRAL_API_KEY`
- `openai_chat` → base_url `https://api.openai.com`, api_key_env `OPENAI_API_KEY`
- `openai_responses` → base_url `https://api.openai.com`, api_key_env `OPENAI_API_KEY`

These are placeholders representing the provider's public endpoint and a conventional env-var name. They are not opinionated picks — they do not select a model id or reasoning effort. `model_id` starts empty; the user types their pick.

`target_ref` is required. If the user enters an existing `target_ref`, the modal shows an inline error ("a target with this ref already exists"). The auto-collision-rename logic in today's `targetRefForCollision` is removed.

### Empty state

First-run state (no models registered, no tier bindings):
- Header strip shows `fast → (none)   standard → (none)   deep → (none)` in dim styling.
- Model table shows a single empty-state row with copy: "No models registered. Click + Register model to add your first one." and a primary `[ + Register model ]` button.
- The "Set up all 3 tiers (recommended)" CTA from today's `TierPresetCard` is removed.

A wake firing against an unbound tier surfaces an inference error at wake time; the engine already enforces this. The frontend does not preempt the user with a "you must configure all tiers" gate.

### Remove constraints

A model row can be removed only when it owns no tier bindings. If the row is bound to one or more tiers, the `⋯ > Remove` action is disabled with tooltip: "Reassign tier(s) F/S/D first."

### Test connection

`⋯ > Test connection` calls a new Tauri command `test_inference_target({ target_ref })` that runs a tiny completion (`"ping"`, ≤ 5s timeout) against the bound target's config and returns `Result<TestResult, Error>` where `TestResult = { ok: bool, latency_ms: u32, sample: Option<String>, error: Option<String> }`. The row shows an inline result line below the model column for ~6s: `● tested ok · 412ms` or `○ failed · 401 unauthorized`.

### API key health

A new Tauri command `inference_env_status({ env_var })` returns `{ present: bool }` by calling `std::env::var(env_var).is_ok()` in the shell process. The frontend calls this once per distinct `api_key_env` when the model list loads, and re-runs after each Register/Edit. The pill renders `● set` (accent-commit color) or `○ missing` (accent-del color).

## Backend touchpoints

Two new Tauri commands in the proxima-shell backend:

1. `inference_env_status(env_var: String) -> { present: bool }` — synchronous `std::env::var` check
2. `test_inference_target(target_ref: String) -> TestResult` — async; resolves the target via the existing engine inference path, sends a minimal completion request with a 5s timeout, returns latency + sample or error

Both commands live alongside the existing inference commands in the shell backend. The engine inference module already has the request path; `test_inference_target` is a thin wrapper that invokes it with a fixed prompt.

No changes to:
- `register_inference_target` / `remove_inference_target` / `bind_inference_target` / `list_inference_targets` / `list_inference_tier_bindings`
- Engine inference path
- Storage migrations (the `inference_target_ref` column on `personality_wake_entries` already exists)

## Frontend changes

### Removed

- `inference-targets-section.tsx` and `inference-targets-section.test.tsx` — replaced by `models-table.tsx` mounted directly from `index.tsx`
- `tier-preset-card.tsx` and `tier-preset-card.test.tsx` — replaced by the model table component
- `tier-bindings-section.tsx` and `tier-bindings-section.test.tsx` — logic absorbed into the model table's tier-radio column
- `DEFAULT_TIER_PRESETS`, `PRESET_TARGET_REFS`, `targetRefForTier`, `defaultPresetForTier`, `targetRefForCollision` in `constants.ts`
- The "Set up all 3 tiers (recommended)" / "Fill missing tiers from defaults" CTA

### Added

- `models-table.tsx` — new component rendering the header strip + model table + tier radios + overflow menu
- `register-model-modal.tsx` — modal wrapping the existing `TargetDraftEditor`
- `test-result-row.tsx` — small inline result component used by Test connection
- `models-table.test.tsx` — covers: render with N models and 3 bindings → correct radios filled; click empty radio → calls `bindInferenceTier`; click filled radio → no-op; remove disabled while tier-bound; empty state copy
- `register-model-modal.test.tsx` — covers: kind selection updates placeholders; duplicate ref shows error; submit calls `registerInferenceTarget`

### Changed

- `index.tsx` — adds the Chat/Embedding sub-tab strip; mounts `models-table.tsx` in Chat tab; mounts existing `EmbeddingSection` unchanged in Embedding tab
- `constants.ts` — removes preset constants, keeps `TIERS`, `TARGET_KIND_OPTIONS`, `kindLabel`, `configSummary`, `configKey`, `sameConfig`, `safeRefPart`, `shortHash`, `nullableString`, `nullableFloat`, `nullableInt`, `TargetDraft`, `draftFromConfig`, `draftForKind`, `configFromDraft`. Adds `KIND_PLACEHOLDERS` constant for the Register modal's per-kind base_url/api_key_env hints.
- `settings.css` — new rules for `.proxima-models-table-v2` (full-width table), `.proxima-tier-radio` (radio chip), `.proxima-tier-summary-header` (top strip), `.proxima-models-overflow` (overflow menu); removes `.proxima-tier-panel*`, `.proxima-tier-row`, `.proxima-tier-summary`, `.proxima-tier-editor`, `.proxima-tier-cta`, `.proxima-tier-list`

### Bindings

Existing `bindings.ts` types (`InferenceTargetTs`, `InferenceTierBindingTs`, `ModelTierTs`, `Owner`) are unchanged. Two new generated commands (`inference_env_status`, `test_inference_target`) appear in `bindings.ts` after running the codegen step. The `client.ts` `EngineClient` interface gains two methods with matching signatures.

## Testing

- New `models-table.test.tsx` and `register-model-modal.test.tsx` cover the v1 UI flows.
- Remove `tier-preset-card.test.tsx` and `tier-bindings-section.test.tsx`.
- Update `inference-targets-section.test.tsx` to reflect the absence of preset CTAs and `<details>` accordion (or remove if the section is fully replaced).
- No backend test changes: existing tests reference `default-standard` as a chosen string literal, not a magic constant; they continue to register their own fixtures.

## Migration

No data migration. Existing user installations that have registered `default-fast` / `default-standard` / `default-deep` target_refs continue working — those refs become ordinary user-named entries in the model table after the rename of the "Custom target (advanced)" surface to "Register model". The tier bindings table is unchanged.

## Risks

- **Removing the "Set up all 3 tiers (recommended)" CTA** raises the bar on first-run: the user must register at least one model and assign all three tier radios before any wake can fire. The empty-state copy must make this explicit. If telemetry later shows first-run drop-off here, consider adding a one-tap "use my OpenAI key for all three tiers" shortcut — but not in v1.
- **Test connection sends a real API request.** It should use a minimal prompt and short timeout to keep cost negligible, and the button must be explicit (no auto-test on row mount).
- **`inference_env_status` reflects the env at process start, not live.** If the user adds an env var to their shell after launching the app, the pill stays `○ missing` until the app restarts. The pill tooltip should mention this.

## Open questions

None blocking. Two deliberate deferrals:

- Wake-trigger override UI — out of scope; lives in Personalities. The model table's "wake overrides: 0" subline is the placeholder.
- Secret-keychain integration — out of scope; v1 stays env-var-based.
