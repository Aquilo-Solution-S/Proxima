# Workspace mode — Phase 1e

**Status:** design
**Date:** 2026-05-09
**Owner:** Heinrich
**Related:**
- `docs/02-memory.md` (directionality rule, Authorship enum, Relation registry)
- `docs/05-actions.md` (event sources author Facts)
- `docs/08-core-and-flavors.md` (flavor macro, registration)
- `docs/superpowers/specs/2026-05-07-personality-as-composed-behaviors.md`
  (`WakeExecutionMode`, `WORKSPACE_TOOL_CATALOG`, the workspace-mode
  flow this spec implements)
- `docs/superpowers/specs/2026-05-09-personality-authorship-edge.md`
  (`core/authored` edge — extended here to cover Fact emits during a
  wake)

## Problem

Today the Code flavor's default Engineer personality wakes on
`proxima-code/commit-summary-v1` Abstractions and emits a
`development-perspective-v1` Perspective — purely substrate. `WakeEntry.execution_mode = Workspace` and
`workspace_tool_palette` are persisted, the Personalities view exposes
both, but `crates/core/src/wake/fire.rs:158-184` short-circuits any
workspace-mode wake with
`failure_reason = "workspace_mode_not_yet_implemented"`. There is no
path for a Personality to actually edit code.

## Decision

Implement workspace mode as a **flavor-supplied `WorkspaceRunner`**
registered through the `proxima_flavor!` macro. The Code flavor
provides the v1 implementation; the runner creates an isolated git
worktree, runs goose with the canonical `developer` extension filtered
by the WakeEntry's `workspace_tool_palette`, and emits two new Facts
through a dedicated event source:

| Fact schema | Records |
|---|---|
| `proxima-code/workspace-run-v1` | "Personality instance X produced branch B at HEAD H from parent P, exit Z." |
| `proxima-code/workspace-decision-v1` | "User decided D on run R at time T." |

`workspace-run-v1` gets `core/authored` from the firing personality's
Root Perspective via the wake-context auto-wire (extended; see related
spec). `workspace-decision-v1` gets `core/derived-from → run-v1`. The
two Facts together form the auditable trail of every workspace
invocation.

## Out of scope

- Container / sandbox isolation. Hard boundary in v1 is the disposable
  worktree plus the `workspace_tool_palette` allowlist. Same trust
  model as running Claude Code locally on your dev machine.
- Auto-merge to a protected branch. `merged` is a user-initiated action
  via a UI button.
- Push to a remote staging namespace.
- Goal-driven manual wake triggers ("fix this test"). Workspace mode
  uses the existing trigger surface.
- Cross-flavor runner switching. One runner per flavor; the dispatcher
  picks via the personality's flavor binding.
- Sweep / expiration of pending runs. Worktrees persist until the user
  decides explicitly.
- Per-WakeEntry `target_branch` override. `target_branch` is per-repo
  (see Schema changes).
- Code Mode optimization for goose. Adapter-level concern; can be added
  later without changing storage.

## Architecture

```
┌─ Engine (crates/core) ──────────────────────────────────┐
│  WorkspaceRunner trait      + workspace_runners regstry │
│  WorkspaceScope enum        + workspace_triggers regstry│
│  set_wake_entries: validate workspace-eligible triggers │
│  wake/fire.rs                                           │
│    └─ if execution_mode == Workspace:                   │
│         scope    = workspace_triggers[trigger_id](mem)  │
│         runner   = workspace_runners[flavor_id]         │
│         prepared = runner.prepare(scope, ...)           │
│         outcome  = adapter.run(cwd = prepared.path)     │
│         run_fact = runner.finalize(prepared, outcome)   │
│  substrate/wake_authorship                              │
│    └─ wake-context auto-wire extended to F emits        │
└─────────────────────────────────────────────────────────┘
                        │ EventIngest
                        ▼
┌─ flavors/code ──────────────────────────────────────────┐
│  workspace_runner/                                      │
│    source.rs    WorkspaceRunnerSource (EventSource)     │
│    runner.rs    impl WorkspaceRunner                    │
│    worktree.rs  git plumbing (add/remove/diff/sha)      │
│    recipe.rs    inject `developer` extension            │
│    decide.rs    apply user decision (rm | merge | mark) │
│  payloads/                                              │
│    workspace_run_v1.rs                                  │
│    workspace_decision_v1.rs                             │
│  migrations/2026MMDD_workspace_*.sql                    │
└─────────────────────────────────────────────────────────┘
                        │
                        ▼
┌─ proxima-shell + flavors/code/frontend ─────────────────┐
│  WorkspaceRunsPanel (registered shell view)             │
│  Tauri commands: list / diff / decide                   │
└─────────────────────────────────────────────────────────┘
```

## Core surface

`crates/core/src/personality/workspace.rs` (new):

```rust
#[async_trait::async_trait]
pub trait WorkspaceRunner: Send + Sync {
    async fn prepare(
        &self,
        input: WorkspacePrepareInput<'_>,
    ) -> Result<WorkspacePreparedRun, WorkspaceRunnerError>;

    async fn finalize(
        &self,
        prepared: WorkspacePreparedRun,
        outcome: WorkspaceOutcome,
    ) -> Result<WorkspaceRunRecord, WorkspaceRunnerError>;
}

pub struct WorkspacePrepareInput<'a> {
    pub invocation_id: Uuid,
    pub owner: &'a Owner,
    pub wake_token: WakeToken,
    pub mcp_url: &'a str,
    pub root_perspective_memory_id: MemoryId,
    pub triggering_memory: &'a TriggeringMemory,
    pub workspace_scope: WorkspaceScope,        // resolved at dispatch
    pub workspace_tool_palette: &'a [String],
    pub recipe_bytes: &'a [u8],
    pub recipe_sha256: &'a str,
}

pub struct WorkspacePreparedRun {
    pub worktree_path: PathBuf,
    pub branch_name: String,
    pub parent_sha: String,
    pub effective_recipe_path: PathBuf,
}

pub struct WorkspaceOutcome {
    pub exit_code: Option<i32>,
    pub stdout_tail: Option<String>,
    pub stderr_tail: Option<String>,
    pub duration_ms: Option<u64>,
}

pub struct WorkspaceRunRecord {
    pub run_memory_id: MemoryId,
    pub head_sha: String,
}
```

### Workspace scope (deterministic, registered, validated)

`workspace_scope` is **not derived ad hoc by the runner** — it is
resolved by the dispatcher at wake-context assembly time using a
flavor-registered scope extractor. This guarantees:

- Every workspace wake has a deterministic, queryable scope id.
- Workspace-eligible triggers are knowable at WakeEntry write time
  (validation rejects unsuitable triggers before the user can save).
- Runners never traverse the edge graph to figure out where they run.

```rust
pub enum WorkspaceScope {
    Repo { repo_id: Uuid },
    // future flavors add variants; runners pattern-match their own
}

/// Function pointer type, registered per-flavor at macro time, that
/// extracts a typed scope from a typed payload.
pub type WorkspaceScopeExtractor =
    fn(memory_id: MemoryId, sidecar_row: &serde_json::Value) -> Result<WorkspaceScope, ScopeError>;
```

Registered through the macro:

```rust
proxima_core::proxima_flavor! {
    name = "proxima-code",
    // ... existing keys ...
    workspace_runner = workspace_runner::CodeWorkspaceRunner,
    workspace_triggers = [
        // (schema_id, scope_extractor)
        ("proxima-code/commit-summary-v1", payloads::commit_summary_repo_scope),
        ("proxima-code/commit-v1",         payloads::commit_repo_scope),
        ("proxima-code/file-revision-v1",  payloads::file_revision_repo_scope),
        ("proxima-code/code-chunk-v1",     payloads::code_chunk_repo_scope),
    ],
}
```

`FlavorRegistry` gains two slots:
- `workspace_runners: HashMap<FlavorId, Arc<dyn WorkspaceRunner>>`
- `workspace_triggers: HashMap<SchemaId, WorkspaceScopeExtractor>`

Both frozen at boot. Looked up at fire time and at WakeEntry
write-validation time.

### Write-time validation

`crates/core/src/inference/set_wake_entries.rs` validates each draft:

> If `execution_mode == Workspace` then
> `registry.workspace_triggers.contains_key(trigger_id)` must hold,
> else reject with
> `WakeEntryError::TriggerNotWorkspaceEligible(trigger_id)`.

UI surfaces the error inline on the WakeEntry editor; the trigger
picker disables non-eligible schemas when `execution_mode = Workspace`
is selected.

`crates/core/src/wake/fire.rs`: replace the
`workspace_mode_not_yet_implemented` short-circuit (lines 158-184) with
the prepare → adapter.run(cwd) → finalize path. The existing
`start_wake_invocation` row, wake-token mint, and adapter invocation
all remain — workspace mode adds two flavor-side hooks around the
existing adapter call. `TargetInvocation` gains an optional
`cwd: Option<PathBuf>` field that the goose adapter passes to
`std::process::Command::current_dir`.

## Schema changes (flavors/code)

### `proxima_code.repos`

```sql
ALTER TABLE proxima_code.repos
    ADD COLUMN target_branch TEXT;
```

No SQL backfill (existing rows get NULL). Going forward,
`register_repo` reads the source repo's HEAD ref name in Rust at
registration time and writes it into the column. NULL = repo cannot
host workspace runs; UI surfaces this state with a "set target
branch" affordance. Mutable later via a new Tauri command
`code_set_repo_target_branch`; future orchestration hooks rewrite
this column to chain runs across integration branches.

### `proxima_code.workspace_run_v1`

```sql
CREATE TABLE proxima_code.workspace_run_v1 (
    memory_id            UUID PRIMARY KEY REFERENCES proxima_core.memories,
    wake_invocation_id   UUID NOT NULL UNIQUE,
    repo_id              UUID NOT NULL,
    target_branch        TEXT NOT NULL,
    worktree_path        TEXT NOT NULL,
    branch_name          TEXT NOT NULL,        -- proxima/wake/<inv>
    parent_sha           TEXT NOT NULL,
    head_sha             TEXT NOT NULL,
    diff_stat_json       JSONB NOT NULL,       -- {files_changed, insertions, deletions, files: [{path, +, -}]}
    exit_code            INTEGER,
    stdout_tail          TEXT,
    stderr_tail          TEXT,
    duration_ms          BIGINT,
    created_at           TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX ON proxima_code.workspace_run_v1 (repo_id);
```

### `proxima_code.workspace_decision_v1`

```sql
CREATE TABLE proxima_code.workspace_decision_v1 (
    memory_id            UUID PRIMARY KEY REFERENCES proxima_core.memories,
    workspace_run_memory_id UUID NOT NULL REFERENCES proxima_core.memories,
    decision             TEXT NOT NULL CHECK (decision IN ('rejected','accepted','merged')),
    decided_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    reason_text          TEXT,
    decided_by_owner_id  UUID NOT NULL
);
CREATE INDEX ON proxima_code.workspace_decision_v1 (workspace_run_memory_id);
```

Both schemas register in `proxima_flavor!` under `fact_schemas`. Fact
identity is UUIDv7 (per inv 17); content hash for EventSourceAuthored
edges referencing them is computed by the existing event-source path.

### Event source

```rust
// flavors/code/src/workspace_runner/source.rs
pub const WORKSPACE_RUNNER_SOURCE_ID: &str = "proxima-code/workspace-runner";

pub struct WorkspaceRunnerSource;

impl EventSource for WorkspaceRunnerSource {
    fn id(&self) -> &'static str { WORKSPACE_RUNNER_SOURCE_ID }
    // push-style: no run_poll; runner emits Facts via the engine's
    // EventIngest path tagged with this source id
}
```

Registered via the existing `event_sources` macro key in the Code
flavor's `proxima_flavor!` invocation.

## Wake fire flow (workspace mode)

```
wake/fire.rs (workspace branch):

  1. flavor_id = engine.registry().flavor_of(personality_instance_id)
  2. runner    = engine.registry().workspace_runner(flavor_id)
                 -> WakeError::NoRunnerForFlavor if missing

  3. mint wake_token         (existing)
  4. start_wake_invocation   (existing)
  5. write effective recipe  (existing path; recipe.rs change wires
                              the developer extension when palette
                              non-empty)

  6. (dispatcher, before runner.prepare):
        scope_extractor = registry.workspace_triggers[trigger_schema_id]
        workspace_scope = scope_extractor(triggering_memory)
        // typed sidecar lookup is O(1); no edge traversal.

  7. prepared = runner.prepare(WorkspacePrepareInput {
                  workspace_scope, ...
              })
        flavors/code/src/workspace_runner/runner.rs:
          a. let WorkspaceScope::Repo { repo_id } = scope
                else WorkspaceRunnerError::WrongScopeKind
          b. load Repo row by repo_id
                target_branch = repos.target_branch
                  NULL -> WorkspaceRunnerError::NoTargetBranch
          c. parent_sha = git -C <canonical_path> rev-parse <target_branch>
          d. branch_name = format!("proxima/wake/{invocation_id}")
          e. worktree_path = ~/.proxima/worktrees/<owner_id>/<invocation_id>
          f. git -C <canonical_path> worktree add -b <branch> \
                 <worktree_path> <parent_sha>
          g. return WorkspacePreparedRun { ... }

  8. outcome = adapter.run(TargetInvocation {
                 recipe_path: prepared.effective_recipe_path,
                 params, max_rounds, env,
                 timeout: per_invocation_timeout(max_rounds),
                 cwd:    Some(prepared.worktree_path.clone()),
             })

  9. run_record = runner.finalize(prepared, outcome)
        flavors/code/src/workspace_runner/runner.rs:
          a. head_sha = git -C <worktree> rev-parse HEAD
          b. diff_stat = git -C <worktree> diff --numstat \
                 <parent_sha>..<head_sha> | parse
          c. EventIngest a workspace-run-v1 Fact via
             WorkspaceRunnerSource:
               authorship  = EventSource(WORKSPACE_RUNNER_SOURCE_ID)
               wake_context = Some(active_wake_token)
          d. substrate sees wake_context, auto-wires:
               core/authored: Root P -> run-Fact
               core/derived-from: run-Fact -> triggering_memory
                                  (via existing read-log behavior)
          e. return WorkspaceRunRecord { run_memory_id, head_sha }

 10. wake_invocation outcome row written  (existing)
 11. wake_token revoked                   (existing)
```

`exit_code != 0`, `head_sha == parent_sha`, and "rounds exhausted" all
emit a run Fact. The run is observable; the user reviews and decides.
Only catastrophic worktree creation failure (step 6.f) skips the Fact
— at that point there is no observable run to record.

## Decision flow (user-driven, post-wake)

```
shell action: workspace_runs_decide(run_id, decision, reason)
  -> Tauri cmd in proxima-shell
     -> code flavor handler in flavors/code/src/workspace_runner/decide.rs:
          1. load workspace_run_v1 row by run_id
          2. match decision {
               Rejected => {
                 git -C <canonical_path> worktree remove --force <worktree>
                 git -C <canonical_path> worktree prune
                 git -C <canonical_path> branch -D <branch_name>
               }
               Accepted => {
                 // no-op on disk; worktree + branch persist
               }
               Merged => {
                 git -C <canonical_path> merge --ff-only <branch_name>
                 // on conflict / non-ff:
                 //   return Err(WorkspaceDecisionError::MergeConflict { stderr })
                 //   no decision Fact emitted; user retries or rejects
                 git -C <canonical_path> worktree remove --force <worktree>
                 git -C <canonical_path> worktree prune
               }
             }
          3. EventIngest a workspace-decision-v1 Fact via
             WorkspaceRunnerSource (authorship = EventSource;
             content carries decided_by_owner_id)
          4. substrate auto-wires core/derived-from: decision -> run
          5. return decision_memory_id
```

The merge action runs `git merge --ff-only` — non-fast-forward or
conflicts return `MergeConflict` to the caller without writing a
decision Fact. User resolves manually in their working repo (or in the
worktree before merging) and retries, or rejects.

## Recipe wiring

`crates/core/src/wake/recipe.rs` (extracted from `fire.rs`'s existing
`write_effective_recipe`):

```rust
pub fn write_effective_recipe(
    bundled_recipe: &[u8],
    mcp_url: &str,
    wake_token: WakeToken,
    substrate_palette: &[String],
    workspace_palette: &[String],   // NEW; empty in substrate-only mode
) -> Result<PathBuf, ProtocolError>;
```

When `workspace_palette` is non-empty the function injects a second
extension entry into the rendered recipe — Goose's built-in
`developer` extension — with `available_tools` derived from the
canonical mapping:

```
proxima-workspace/text_editor  ->  developer__text_editor
proxima-workspace/shell        ->  developer__shell
proxima-workspace/list_files   ->  developer__list_files
```

The mapping table lives next to the existing substrate-palette
plumbing. `WORKSPACE_TOOL_CATALOG` in `personality/mod.rs` stays the
canonical declaration; recipe.rs holds the goose-adapter-specific
mapping.

If `execution_mode == Workspace` but `workspace_palette` is empty, the
worktree is still created (the run is observable) but no developer
extension is injected — the wake degenerates to a substrate-only run
inside a worktree. Treated as a no-op rather than an error so the
palette and execution_mode can be edited independently.

## Worktree lifecycle

```
~/.proxima/worktrees/{owner_id}/{invocation_id}/
```

| Lifecycle event | Action |
|---|---|
| Wake fire | `git worktree add -b proxima/wake/{inv} <path> {parent_sha}` |
| Goose run finishes (any exit) | Read `head_sha`, `diff_stat`, leave on disk |
| `workspace-run-v1` emitted | Path + branch_name in sidecar; Fact is the canonical pointer |
| Decision = `rejected` | `worktree remove --force` + `worktree prune` + `branch -D` |
| Decision = `accepted` | No on-disk change; worktree + branch persist indefinitely |
| Decision = `merged` | `merge --ff-only` into target_branch → on success, `worktree remove` + `prune` |
| Worktree creation fails | No Fact emitted; wake_invocation row records `failure_reason` |

No automatic GC. Worktrees from accepted-but-not-merged runs accumulate
on disk; the v1 user manages the directory manually if it gets large.

## Authorship edge — Fact emit during wake

The existing `core/authored` spec
(`2026-05-09-personality-authorship-edge.md`) covers Abstraction and
Perspective emits via the substrate tools `core/emit_abstraction` /
`core/emit_perspective`. Workspace mode requires the same auto-wire
for **Fact** emits when an event source ingests a Fact within an
active wake context. The directionality rule allows P → F (m=2,
n=0, m ≥ n), so the edge shape is unchanged.

The extension is documented as an addendum in the authorship-edge
spec; this spec only depends on the contract:

> Any Memory emitted while a wake_token is in scope receives one
> `core/authored` edge from that wake's snapshotted
> `current_root_perspective_memory_id`, regardless of whether the
> emit path is a substrate tool or `EventIngest`.

The wake-token-aware EventIngest path threads the wake context into
storage's append path the same way the substrate tools already do.

## Shell UX

`flavors/code/frontend/src/workspace-runs-panel.tsx` (new), registered
alongside `ReposPanel` in the Code flavor's frontend init.

```
┌─ Workspace Runs ─────────────────────────────────────────┐
│  [Pending 3]  [Reviewed 12]                              │
│                                                          │
│  ┌─ pending ───────────────────────────────────────────┐ │
│  │ Engineer · proxima-code · 2026-05-09 14:23          │ │
│  │ branch: proxima/wake/019f...                        │ │
│  │ +47 −12 in 4 files · exit 0 · 2.3s                  │ │
│  │ [View diff] [Reject] [Accept] [Merge → road-to-v1]  │ │
│  └─────────────────────────────────────────────────────┘ │
└──────────────────────────────────────────────────────────┘
```

`[View diff]` shells out via Tauri to `git -C <worktree> diff
<parent>..<head>` and renders in a `<pre>` block. No syntax
highlighting in v1.

`[Merge → <branch>]` reads `target_branch` from the source repo's row;
button is disabled if `target_branch IS NULL` (with hover tooltip
pointing to the repos panel).

## Tauri command surface

| Command | Args | Returns |
|---|---|---|
| `workspace_runs_list` | `{ status: "pending" \| "reviewed" \| null }` | `WorkspaceRunTs[]` |
| `workspace_run_diff` | `{ run_id: Uuid }` | `{ unified_diff: string }` |
| `workspace_runs_decide` | `{ run_id: Uuid, decision: enum, reason: string \| null }` | `{ decision_memory_id: Uuid }` \| `{ kind: "merge_conflict", message: string }` |
| `code_set_repo_target_branch` | `{ repo_id: Uuid, target_branch: string \| null }` | `RepoRecordTs` |

All four are thin wrappers over flavor-side functions in
`flavors/code/src/workspace_runner/` plus a graph query for the list
verb. `pending` = run Fact exists but no decision Fact references it;
`reviewed` = decision Fact exists.

## Failure modes

| Failure | Behavior |
|---|---|
| WakeEntry write with workspace mode + non-eligible trigger | Validation error at `set_wake_entries` time; row never persisted; UI surfaces inline |
| Workspace mode wake fires but `workspace_triggers` registry has no extractor for trigger schema (panic guard — should be unreachable post-validation) | Wake fails fast with `WorkspaceRunnerError::TriggerNotEligible`; no Fact |
| Scope extractor returns the wrong scope kind for the runner | `WorkspaceRunnerError::WrongScopeKind`; wake fails; no Fact |
| `proxima-code/repos` has no row for `repo_id` | `NoSuchRepo`; wake fails; no Fact |
| `repos.target_branch` is NULL | `NoTargetBranch`; wake fails; no Fact |
| `git rev-parse <target_branch>` fails | `TargetBranchInvalid`; wake fails; no Fact |
| `git worktree add` fails | `WorktreeCreate { stderr }`; wake fails; no Fact |
| Goose subprocess exits non-zero | Run Fact emitted (`exit_code != 0`, `stderr_tail` populated). User reviews diff (possibly empty) and decides |
| `head_sha == parent_sha` | Run Fact emitted with empty diff_stat. UI labels "no changes"; user decides (typically reject) |
| Adapter reports rounds exhausted | Run Fact emitted; same review path |
| Merge action: non-ff or conflict | Tauri returns `MergeConflict { stderr }`; no decision Fact emitted |

## Tests

| Layer | Test |
|---|---|
| `flavors/code/src/workspace_runner/worktree.rs` | unit: create + remove against tempdir-init'd repo |
| `flavors/code/src/workspace_runner/recipe.rs` | unit: developer extension injected with mapped `available_tools`; not injected for empty palette |
| `flavors/code/tests/workspace_run_pg.rs` (new) | integration with Postgres + tempdir repo: register repo with `target_branch=main` → fire workspace wake using a no-op recipe (`echo`-only goose adapter mock) → assert `workspace-run-v1` Fact + `core/authored` edge from the firing personality's Root P + `core/derived-from` edge to triggering memory |
| `flavors/code/tests/workspace_decide_pg.rs` (new) | for each decision: assert `workspace-decision-v1` Fact + `core/derived-from` edge to run Fact + on-disk side effects (remove for reject/merge, no-op for accept) |
| `flavors/code/tests/workspace_merge_conflict.rs` (new) | introduce a conflicting commit on `target_branch` between fire and decide → merge action returns `MergeConflict`; no decision Fact written |
| `apps/proxima-shell/src-tauri/src/commands/workspace.rs` | tauri-cmd unit tests modeled on `commands/repo_ingest.rs` |
| `flavors/code/frontend/src/workspace-runs-panel.test.tsx` | renders pending/reviewed; dispatches the four actions (view/reject/accept/merge); mirrors `repos-panel.test.tsx` |
| `crates/core/src/personality/workspace.rs` | unit: `FlavorRegistry` returns `NoRunnerForFlavor` when a personality's flavor has no runner registered |
| `crates/core/src/inference/set_wake_entries.rs` | unit: workspace-mode WakeEntry with non-eligible trigger → `TriggerNotWorkspaceEligible`; eligible trigger → write succeeds; substrate-only mode bypasses the check |
| `crates/core/src/wake/dispatch.rs` | unit: scope extractor is invoked at context assembly; `WorkspacePrepareInput.workspace_scope` is populated from the typed sidecar; non-workspace wakes don't invoke the extractor |

## Migration

Three timestamped migrations in `flavors/code/migrations` (filename
convention `YYYYMMDDhhmmss_<topic>.sql`, latest existing is
`20260506000120_personality_self_schemas.sql`):

1. `<ts>_repos_target_branch.sql` — `ALTER TABLE proxima_code.repos
   ADD COLUMN target_branch TEXT`. No backfill; existing rows have
   NULL target_branch and are ineligible for workspace mode until the
   user sets it via the Tauri verb.
2. `<ts+1>_workspace_run_v1.sql` — table + index per schema above.
3. `<ts+2>_workspace_decision_v1.sql` — table + index per schema above.

No core migrations beyond what the wake-context-aware EventIngest
extension needs (covered in the related authorship-edge addendum).

## Phasing

Four implementable steps; each lands as its own plan in `.plans/`:

1. **Core seam.** Add `WorkspaceRunner` trait + `workspace_runners`
   registry slot, `proxima_flavor!` macro key for runner registration,
   `TargetInvocation.cwd`, wake-context-aware EventIngest. Workspace
   branch in `wake/fire.rs` becomes "dispatch to runner", but the Code
   flavor ships an empty `Unimplemented` runner so the e2e behavior is
   unchanged. Tests at the trait level only.

2. **Workspace-trigger registry + scope plumbing + write-time
   validation.** Add `WorkspaceScope` enum, `WorkspaceScopeExtractor`
   type, `workspace_triggers` registry slot, `proxima_flavor!` macro
   key for trigger registration. Code flavor registers extractors for
   its four `repo_id`-bearing schemas. `set_wake_entries` validates
   workspace-mode entries against the registry. Dispatcher resolves
   scope at wake-context assembly time and threads
   `WorkspacePrepareInput.workspace_scope`. WakeEntry editor frontend
   filters trigger picker by eligibility. No runtime worktree behavior
   yet — the `Unimplemented` runner from phase 1 still returns its
   sentinel; phase 2 lands the determinism guarantees independently.

3. **Code flavor runner.** Implement `CodeWorkspaceRunner`:
   `worktree.rs`, `recipe.rs`, `runner.rs`, payloads, migrations
   (target_branch + run + decision tables), `WorkspaceRunnerSource`,
   the `workspace-run-v1` emit. End-to-end integration test fires a
   workspace wake with a no-op recipe and asserts the Fact + edges
   land. No decision UI yet — accepted/rejected/merged not exposed.

4. **Decision UX.** `decide.rs` + Tauri commands +
   `WorkspaceRunsPanel` + frontend tests. Merge-conflict handling and
   the `code_set_repo_target_branch` verb close the loop.

Each step is reviewable against the spec independently; merging order
is 1 → 2 → 3 → 4 (no cyclic dependencies between steps).
