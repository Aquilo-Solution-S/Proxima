import "./surface.css";

import {
  For,
  Show,
  createMemo,
  createSignal,
  onCleanup,
  type Component,
  type JSX,
} from "solid-js";
import { SchemaTag, Mono } from "../primitives";
import { useGraph, type DecodedMemory } from "../graph-store";
import { useGraphFilter } from "../graph-filter-store";
import { filterGraphSnapshot } from "../graph-selectors";
import type { Hub } from "../hub";
import {
  commands,
  type GoalDraft,
  type GoalRow,
  type MemoryRow,
} from "../bindings";
import { EventStream } from "./surface-events";
import { GoalDialog } from "./goal-dialog";

const GOAL_RAIL_DEFAULT_WIDTH = 280;
const GOAL_RAIL_MIN_WIDTH = 220;
const GOAL_RAIL_MAX_WIDTH = 560;
const SURFACE_CENTER_MIN_WIDTH = 300;
const RAIL_COLLAPSED_WIDTH = 48;

// ── Goal rail ───────────────────────────────────────────────────────────
const renderGoalPayload = (goal: GoalRow, hub: Hub): JSX.Element | null => {
  const renderer = hub.rendererFor(
    goal.schema_id,
    goal.schema_version,
    "Goal",
  );
  if (renderer === null) return null;
  const codec = hub.codecFor(goal.schema_id, goal.schema_version);
  let payload: unknown;
  try {
    payload = codec === null
      ? null
      : codec.decode(new Uint8Array(goal.payload));
  } catch {
    return null;
  }
  if (payload === null) return null;
  const synthetic: MemoryRow = {
    id: goal.id,
    kind: "Goal",
    schema_id: goal.schema_id,
    schema_version: goal.schema_version,
    owner: goal.owner,
    payload: goal.payload,
  };
  return renderer.render({ memory: synthetic, payload });
};

const stateClass = (state: GoalRow["state"]): string =>
  `state-${state.toLowerCase()}`;

const GoalPayloadBody: Component<{
  goal: GoalRow;
  hub: Hub;
}> = (props) => {
  const rendered = (): JSX.Element | null =>
    renderGoalPayload(props.goal, props.hub);
  return (
    <Show keyed when={rendered()} fallback={<p class="prose prose-small">{props.goal.text}</p>}>
      {(node) => node}
    </Show>
  );
};

const ProposedGoalCard: Component<{
  goal: GoalRow;
  hub: Hub;
  onAfterWrite: () => void;
  onModify: (goal: GoalRow) => void;
}> = (props) => {
  const [busy, setBusy] = createSignal(false);
  const writeWithState = async (state: "Active" | "Rejected") => {
    setBusy(true);
    try {
      const draft: GoalDraft = {
        owner: props.goal.owner,
        schema_id: props.goal.schema_id,
        schema_version: props.goal.schema_version,
        text: props.goal.text,
        payload: props.goal.payload,
        state,
        parent_goal_ids: props.goal.parent_goal_ids,
        supersedes_goal_id: props.goal.id,
        authorship: "User",
        request_id: `goal-rail:${state}:${props.goal.id}:${Date.now()}`,
      };
      const result = await commands.goalWrite(draft);
      if (result.status === "error") throw result.error;
      props.onAfterWrite();
    } finally {
      setBusy(false);
    }
  };
  return (
    <article class="goal-proposed-card" title={props.goal.id}>
      <div class="goal-proposed-head">
        <SchemaTag
          id={props.goal.schema_id}
          version={props.goal.schema_version}
        />
        <span class="state-pill is-proposed">Proposed</span>
      </div>
      <div class="goal-proposed-body">
        <GoalPayloadBody goal={props.goal} hub={props.hub} />
      </div>
      <div class="goal-proposed-actions">
        <button
          type="button"
          class="goal-action-icon is-accept"
          aria-label="Accept proposal"
          title="Accept"
          disabled={busy()}
          onClick={() => void writeWithState("Active")}
        >
          ✓
        </button>
        <button
          type="button"
          class="goal-action-icon is-modify"
          aria-label="Modify proposal"
          title="Modify"
          disabled={busy()}
          onClick={() => props.onModify(props.goal)}
        >
          ✎
        </button>
        <button
          type="button"
          class="goal-action-icon is-decline"
          aria-label="Decline proposal"
          title="Decline"
          disabled={busy()}
          onClick={() => void writeWithState("Rejected")}
        >
          ✗
        </button>
      </div>
    </article>
  );
};

const GoalCard: Component<{
  goal: GoalRow;
  hub: Hub;
}> = (props) => (
  <article class="goal-card" title={props.goal.id}>
    <div class="goal-meta">
      <span class={`state-pill ${stateClass(props.goal.state)}`}>
        {props.goal.state}
      </span>
      <SchemaTag
        id={props.goal.schema_id}
        version={props.goal.schema_version}
      />
    </div>
    <div class="goal-card-body">
      <GoalPayloadBody goal={props.goal} hub={props.hub} />
    </div>
  </article>
);

const GoalRail: Component<{
  collapsed: boolean;
  onToggle: () => void;
  width: number;
  onResizeStart: JSX.EventHandlerUnion<HTMLDivElement, PointerEvent>;
  onResizeKeyDown: JSX.EventHandlerUnion<HTMLDivElement, KeyboardEvent>;
  goals: GoalRow[];
  hub: Hub;
  onAfterWrite: () => void;
  onAddGoal: () => void;
  onModifyProposal: (goal: GoalRow) => void;
}> = (props) => {
  const proposed = (): GoalRow[] =>
    props.goals.filter((goal) => goal.state === "Proposed");
  const accepted = (): GoalRow[] =>
    props.goals.filter((goal) =>
      goal.state !== "Proposed" && goal.state !== "Rejected"
    );
  return (
    <aside
      classList={{
        "goal-rail": true,
        "is-collapsed": props.collapsed,
      }}
    >
      <Show
        when={!props.collapsed}
        fallback={
          <button
            type="button"
            class="rail-collapsed-trigger"
            aria-label="Expand Goal DAG"
            aria-expanded="false"
            onClick={props.onToggle}
          >
            <span class="rail-collapse-icon is-closed" aria-hidden="true" />
            <span class="rail-collapsed-title">Goal DAG</span>
          </button>
        }
      >
        <div class="rail-head">
          <div class="rail-head-copy">
            <span class="rail-title">Goal DAG</span>
            <Mono style={{ "font-size": "9px", color: "var(--ink-50)" }}>
              supersession-only
            </Mono>
          </div>
          <div class="rail-head-actions">
            <button
              type="button"
              class="rail-add"
              aria-label="Add goal"
              title="Add goal"
              onClick={props.onAddGoal}
            >
              +
            </button>
            <button
              type="button"
              class="rail-toggle"
              aria-label="Collapse Goal DAG"
              aria-expanded="true"
              onClick={props.onToggle}
            >
              <span class="rail-collapse-icon is-left" aria-hidden="true" />
            </button>
          </div>
        </div>
        <div
          class="goal-rail-resize-handle"
          role="separator"
          aria-label="Resize Goal DAG"
          aria-orientation="vertical"
          aria-valuemin={GOAL_RAIL_MIN_WIDTH}
          aria-valuemax={GOAL_RAIL_MAX_WIDTH}
          aria-valuenow={props.width}
          tabIndex={0}
          onPointerDown={props.onResizeStart}
          onKeyDown={props.onResizeKeyDown}
        />
        <div class="goal-list">
          <Show when={accepted().length > 0}>
            <section class="goal-accepted-section" aria-label="Accepted goals">
              <For each={accepted()}>
                {(goal) => <GoalCard goal={goal} hub={props.hub} />}
              </For>
            </section>
          </Show>
          <Show when={proposed().length > 0}>
            <section class="goal-proposed-section" aria-label="Proposed goals">
              <header class="goal-proposed-section-head">
                <Mono style={{ "font-size": "9px", color: "var(--ink-50)" }}>
                  Proposed · {proposed().length}
                </Mono>
              </header>
              <For each={proposed()}>
                {(goal) => (
                  <ProposedGoalCard
                    goal={goal}
                    hub={props.hub}
                    onAfterWrite={props.onAfterWrite}
                    onModify={props.onModifyProposal}
                  />
                )}
              </For>
            </section>
          </Show>
          <Show when={accepted().length === 0}>
            <p class="proxima-dim">
              {proposed().length === 0 ? "No goals" : "No accepted goals yet"}
            </p>
          </Show>
        </div>
      </Show>
    </aside>
  );
};

// ── Traversal lanes (F→A→P) ───────────────────────────────────────────
const renderMemoryPayload = (
  memory: DecodedMemory,
  hub: Hub,
): JSX.Element | null => {
  const renderer = hub.rendererFor(
    memory.row.schema_id,
    memory.row.schema_version,
    memory.row.kind,
  );
  return renderer?.render({
    memory: memory.row,
    payload: memory.payload,
  }) ?? null;
};

const shortId = (id: string): string => id.slice(0, 8);

const MemoryCard: Component<{ memory: DecodedMemory; hub: Hub }> = (props) => {
  const rendered = (): JSX.Element | null =>
    renderMemoryPayload(props.memory, props.hub);
  return (
    <article
      class={props.memory.row.kind === "Perspective" ? "p-card" : "a-card"}
      title={props.memory.row.id}
    >
      <div class="card-head">
        <SchemaTag
          id={props.memory.row.schema_id}
          version={props.memory.row.schema_version}
        />
      </div>
      <Show
        keyed
        when={rendered()}
        fallback={
          <p class="prose prose-small">
            {props.memory.decodeError?.message ??
              `${props.memory.row.payload.length} payload bytes`}
          </p>
        }
      >
        {(node) => node}
      </Show>
      <div class="card-foot">
        <Mono style={{ "font-size": "9px", color: "var(--ink-40)" }}>
          {shortId(props.memory.row.id)}
        </Mono>
      </div>
    </article>
  );
};

const MemoryExplorer: Component<{
  hub: Hub;
  memories: DecodedMemory[];
  label: string;
  glyph: string;
}> = (props) => {
  const [selectedId, setSelectedId] = createSignal<string | null>(null);
  const selectedMemory = createMemo(
    () =>
      props.memories.find((memory) => memory.row.id === selectedId()) ??
      props.memories[0] ??
      null,
  );

  return (
    <div class="memory-explorer">
      <div class="fact-list" role="listbox" aria-label={props.label}>
        <For each={props.memories}>
          {(memory) => (
            <button
              type="button"
              classList={{
                "fact-list-item": true,
                "is-selected": selectedMemory()?.row.id === memory.row.id,
              }}
              role="option"
              aria-selected={selectedMemory()?.row.id === memory.row.id}
              title={memory.row.schema_id}
              onClick={() => setSelectedId(memory.row.id)}
            >
              <span class="fact-list-glyph" aria-hidden="true">
                {props.glyph}
              </span>
              <span class="fact-list-copy">
                <span class="fact-list-schema">{memory.row.schema_id}</span>
                <span class="fact-list-meta">
                  {shortId(memory.row.id)} · {memory.row.payload.length} bytes
                </span>
              </span>
            </button>
          )}
        </For>
      </div>

      <Show keyed when={selectedMemory()}>
        {(memory) => {
          const rendered = (): JSX.Element | null =>
            renderMemoryPayload(memory, props.hub);
          return (
            <article class="fact-detail">
              <div class="fact-detail-head">
                <SchemaTag
                  id={memory.row.schema_id}
                  version={memory.row.schema_version}
                />
                <Mono style={{ "font-size": "9px", color: "var(--ink-40)" }}>
                  {shortId(memory.row.id)}
                </Mono>
              </div>
              <div class="fact-detail-body">
                <Show
                  keyed
                  when={rendered()}
                  fallback={
                    <p class="prose prose-small">
                      {memory.decodeError?.message ??
                        `${memory.row.payload.length} payload bytes`}
                    </p>
                  }
                >
                  {(node) => node}
                </Show>
              </div>
              <div class="card-foot">
                <Mono style={{ "font-size": "9px", color: "var(--ink-40)" }}>
                  {memory.row.id}
                </Mono>
              </div>
            </article>
          );
        }}
      </Show>
    </div>
  );
};

const LayerHeader: Component<{
  contentId: string;
  glyph: string;
  name: string;
  count: number;
  detail: string;
  collapsed: boolean;
  onToggle: () => void;
}> = (props) => (
  <button
    type="button"
    class="lane-toggle"
    aria-label={`${props.collapsed ? "Expand" : "Collapse"} ${
      props.name
    } section`}
    aria-expanded={!props.collapsed}
    aria-controls={props.contentId}
    onClick={props.onToggle}
  >
    <span class="lane-label">
      <span class="lane-letter">{props.glyph}</span>
      <span class="lane-meta">
        <span class="lane-name">{props.name}</span>
        <span class="lane-count">{props.count}</span>
        <Mono style={{ "font-size": "9px", color: "var(--ink-40)" }}>
          {props.detail}
        </Mono>
      </span>
    </span>
    <span
      classList={{
        "lane-collapse-icon": true,
        "is-collapsed": props.collapsed,
      }}
      aria-hidden="true"
    />
  </button>
);

const TraversalLanes: Component<{ hub: Hub; memories: DecodedMemory[] }> = (
  props,
) => {
  const [perspectivesCollapsed, setPerspectivesCollapsed] =
    createSignal(false);
  const [abstractionsCollapsed, setAbstractionsCollapsed] =
    createSignal(false);
  const [factsCollapsed, setFactsCollapsed] = createSignal(false);
  const facts = () => props.memories.filter((m) => m.row.kind === "Fact");
  const abstractions = () =>
    props.memories.filter((m) => m.row.kind === "Abstraction");
  const perspectives = () =>
    props.memories.filter((m) => m.row.kind === "Perspective");

  return (
    <div class="traversal">
      {/* PERSPECTIVE LANE */}
      <section
        classList={{
          lane: true,
          "lane-p": true,
          "is-collapsed": perspectivesCollapsed(),
        }}
      >
        <LayerHeader
          contentId="surface-perspectives-content"
          glyph="P"
          name="Perspective"
          count={perspectives().length}
          detail="causal claim carrier"
          collapsed={perspectivesCollapsed()}
          onToggle={() => setPerspectivesCollapsed((v) => !v)}
        />
        <Show when={!perspectivesCollapsed()}>
          <div id="surface-perspectives-content" class="lane-content">
            <Show
              when={perspectives().length > 0}
              fallback={<p class="proxima-dim">No perspectives</p>}
            >
              <For each={perspectives()}>
                {(memory) => <MemoryCard memory={memory} hub={props.hub} />}
              </For>
            </Show>
          </div>
        </Show>
      </section>

      {/* P → A connector */}
      <div class="lane-connector">
        <Mono
          style={{
            "font-size": "9px",
            color: "var(--ink-40)",
            "line-height": "32px",
            "text-align": "center",
          }}
        >
          A → P (provenance)
        </Mono>
      </div>

      {/* ABSTRACTION LANE */}
      <section
        classList={{
          lane: true,
          "lane-a": true,
          "is-collapsed": abstractionsCollapsed(),
        }}
      >
        <LayerHeader
          contentId="surface-abstractions-content"
          glyph="A"
          name="Abstractions"
          count={abstractions().length}
          detail="authored prose + typed scaffolding"
          collapsed={abstractionsCollapsed()}
          onToggle={() => setAbstractionsCollapsed((v) => !v)}
        />
        <Show when={!abstractionsCollapsed()}>
          <div
            id="surface-abstractions-content"
            class="lane-content lane-content-row"
          >
            <Show
              when={abstractions().length > 0}
              fallback={<p class="proxima-dim">No abstractions</p>}
            >
              <MemoryExplorer
                hub={props.hub}
                memories={abstractions()}
                label="Abstractions"
                glyph="A"
              />
            </Show>
          </div>
        </Show>
      </section>

      {/* A → F connector */}
      <div class="lane-connector">
        <Mono
          style={{
            "font-size": "9px",
            color: "var(--ink-40)",
            "line-height": "32px",
            "text-align": "center",
          }}
        >
          F → A (provenance)
        </Mono>
      </div>

      {/* FACTS LANE */}
      <section
        classList={{
          lane: true,
          "lane-f": true,
          "is-collapsed": factsCollapsed(),
        }}
      >
        <LayerHeader
          contentId="surface-facts-content"
          glyph="F"
          name="Facts"
          count={facts().length}
          detail="source_batch - F→A is intra-batch"
          collapsed={factsCollapsed()}
          onToggle={() => setFactsCollapsed((v) => !v)}
        />
        <Show when={!factsCollapsed()}>
          <div
            id="surface-facts-content"
            class="lane-content lane-content-row"
          >
            <Show
              when={facts().length > 0}
              fallback={<p class="proxima-dim">No facts</p>}
            >
              <MemoryExplorer
                hub={props.hub}
                memories={facts()}
                label="Facts"
                glyph="F"
              />
            </Show>
          </div>
        </Show>
      </section>
    </div>
  );
};

// ── FullSurface ─────────────────────────────────────────────────────────
export const FullSurface: Component<{ hub: Hub }> = (props) => {
  const graph = useGraph();
  const filters = useGraphFilter();
  const [goalsCollapsed, setGoalsCollapsed] = createSignal(true);
  const [eventsCollapsed, setEventsCollapsed] = createSignal(true);
  const [goalRailWidth, setGoalRailWidth] = createSignal(GOAL_RAIL_DEFAULT_WIDTH);
  const [resizingGoals, setResizingGoals] = createSignal(false);
  const filtered = createMemo(() =>
    filterGraphSnapshot(graph.state(), filters.state(), props.hub),
  );
  const memories = () => filtered().memories;
  const events = () =>
    Array.from(graph.state().eventsBySeq.values()).sort((a, b) =>
      a.seq < b.seq ? 1 : -1,
    );
  const goals = () => filtered().goals;
  const [dialogState, setDialogState] = createSignal<
    { mode: "create" } | { mode: "modify"; goal: GoalRow } | null
  >(null);
  let surfaceRef!: HTMLDivElement;
  let stopGoalResize: (() => void) | null = null;

  const surfaceBodyWidth = (): number =>
    surfaceRef === undefined || surfaceRef.clientWidth <= 0
      ? 1180
      : surfaceRef.clientWidth;

  const eventRailWidth = (): number => {
    if (eventsCollapsed()) return RAIL_COLLAPSED_WIDTH;
    const bodyWidth = surfaceBodyWidth();
    return Math.min(Math.max(bodyWidth * 0.32, 220), 380);
  };

  const clampGoalRailWidth = (width: number): number => {
    const maxByBody = Math.max(
      GOAL_RAIL_MIN_WIDTH,
      surfaceBodyWidth() - eventRailWidth() - SURFACE_CENTER_MIN_WIDTH - 2,
    );
    return Math.max(
      GOAL_RAIL_MIN_WIDTH,
      Math.min(width, GOAL_RAIL_MAX_WIDTH, maxByBody),
    );
  };

  const startGoalResize = (event: PointerEvent) => {
    if (event.button !== 0 || goalsCollapsed()) return;
    event.preventDefault();
    stopGoalResize?.();

    const startX = event.clientX;
    const rail = (event.currentTarget as HTMLElement).closest(".goal-rail");
    const measuredWidth = rail instanceof HTMLElement
      ? rail.getBoundingClientRect().width
      : 0;
    const startWidth = measuredWidth > 0 ? measuredWidth : goalRailWidth();
    const previousCursor = document.body.style.cursor;
    const previousUserSelect = document.body.style.userSelect;

    setResizingGoals(true);
    document.body.style.cursor = "col-resize";
    document.body.style.userSelect = "none";

    const onPointerMove = (moveEvent: PointerEvent) => {
      setGoalRailWidth(
        clampGoalRailWidth(startWidth + (moveEvent.clientX - startX)),
      );
    };
    const onPointerUp = () => {
      stopGoalResize?.();
    };

    stopGoalResize = () => {
      window.removeEventListener("pointermove", onPointerMove);
      window.removeEventListener("pointerup", onPointerUp);
      document.body.style.cursor = previousCursor;
      document.body.style.userSelect = previousUserSelect;
      setResizingGoals(false);
      stopGoalResize = null;
    };

    window.addEventListener("pointermove", onPointerMove);
    window.addEventListener("pointerup", onPointerUp);
  };

  const resizeGoalRailByKey = (event: KeyboardEvent) => {
    if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
    event.preventDefault();
    const direction = event.key === "ArrowRight" ? 1 : -1;
    const step = event.shiftKey ? 40 : 16;
    setGoalRailWidth((width) =>
      clampGoalRailWidth(width + direction * step)
    );
  };

  onCleanup(() => {
    stopGoalResize?.();
  });

  return (
    <div class="proxima-shell">
      <div
        ref={surfaceRef}
        style={`--surface-goal-width: ${goalRailWidth()}px`}
        classList={{
          "surface-body": true,
          "is-goals-collapsed": goalsCollapsed(),
          "is-events-collapsed": eventsCollapsed(),
          "is-resizing-goals": resizingGoals(),
        }}
      >
        <GoalRail
          collapsed={goalsCollapsed()}
          width={goalRailWidth()}
          onResizeStart={startGoalResize}
          onResizeKeyDown={resizeGoalRailByKey}
          goals={goals()}
          hub={props.hub}
          onAfterWrite={() => void graph.refresh()}
          onToggle={() => setGoalsCollapsed((v) => !v)}
          onAddGoal={() => setDialogState({ mode: "create" })}
          onModifyProposal={(goal) =>
            setDialogState({ mode: "modify", goal })
          }
        />
        <TraversalLanes hub={props.hub} memories={memories()} />
        <EventStream
          collapsed={eventsCollapsed()}
          events={events()}
          hub={props.hub}
          onToggle={() => setEventsCollapsed((v) => !v)}
        />
      </div>
      <Show when={dialogState()} keyed>
        {(state) => (
          <GoalDialog
            hub={props.hub}
            proposal={state.mode === "modify" ? state.goal : undefined}
            onClose={() => setDialogState(null)}
            onAfterWrite={() => void graph.refresh()}
          />
        )}
      </Show>
    </div>
  );
};
