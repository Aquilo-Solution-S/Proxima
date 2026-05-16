import "./surface.css";
import { Show, createEffect, createMemo, createSignal, onCleanup, onMount, type Component } from "solid-js";
import { useGraph } from "../graph-store";
import { useGraphFilter } from "../graph-filter-store";
import { filterGraphSnapshot, oneHopLineage } from "../graph-selectors";
import type { Hub } from "../hub";
import { commands, type GoalDraft, type GoalRow } from "../bindings";
import type { DecodedMemory } from "../graph-store";
import { ActivityStrip, type EngineState } from "./surface/activity-strip";
import { ChipRail } from "./surface/chip-rail";
import { DetailPane } from "./surface/detail-pane";
import { FilterDrawer, type FilterFacets } from "./surface/filter-drawer";
import { RequestsStrip } from "./surface/requests-strip";
import { RowList, type ActiveTab } from "./surface/row-list";
import { TabStrip } from "./surface/tab-strip";
import { installSurfaceKeys } from "./surface/keys";
import { EventStream } from "./surface-events";
import { GoalDialog } from "./goal-dialog";

const goalToDecodedMemory = (goal: GoalRow): DecodedMemory => ({
  row: {
    id: goal.id,
    kind: "Goal",
    schema_id: goal.schema_id,
    schema_version: goal.schema_version,
    owner: goal.owner,
    payload: goal.payload,
  },
  payload: {},
});

const tabLayer = (tab: ActiveTab): "Fact" | "Abstraction" | "Perspective" | "Goal" | null =>
  tab === "All" ? null : tab;

const STATE_FROM_STREAM: Record<string, EngineState> = {
  connecting: "idle",
  live: "idle",
  degraded: "error",
  stopped: "error",
};

export const FullSurface: Component<{ hub: Hub }> = (props) => {
  const graph = useGraph();
  const filter = useGraphFilter();
  const [activeTab, setActiveTab] = createSignal<ActiveTab>("All");
  const [drawerOpen, setDrawerOpen] = createSignal(false);
  const [eventsOpen, setEventsOpen] = createSignal(false);
  const [selectedId, setSelectedId] = createSignal<string | null>(null);
  const [acceptProposal, setAcceptProposal] = createSignal<GoalRow | null>(null);
  const [decliningId, setDecliningId] = createSignal<string | null>(null);
  const [requestsError, setRequestsError] = createSignal<string | null>(null);

  const filtered = createMemo(() => {
    const layer = tabLayer(activeTab());
    const baseFilter = filter.state();
    const adjusted = layer === null
      ? baseFilter
      : { ...baseFilter, layers: new Set([layer]) };
    return filterGraphSnapshot(graph.state(), adjusted, props.hub);
  });

  const counts = createMemo(() => {
    const all = graph.state().memoriesById.size + graph.state().goalsById.size;
    let fact = 0, abs = 0, per = 0;
    for (const m of graph.state().memoriesById.values()) {
      if (m.row.kind === "Fact") fact++;
      else if (m.row.kind === "Abstraction") abs++;
      else if (m.row.kind === "Perspective") per++;
    }
    return {
      All: all,
      Fact: fact,
      Abstraction: abs,
      Perspective: per,
      Goal: graph.state().goalsById.size,
    };
  });

  const facets = createMemo<FilterFacets>(() => {
    const flavors = new Set<string>();
    const schemas = new Map<string, { schemaId: string; flavor: string | null }>();
    const authors = new Set<string>();
    for (const m of graph.state().memoriesById.values()) {
      schemas.set(m.row.schema_id, {
        schemaId: m.row.schema_id,
        flavor: props.hub.flavorFor(m.row.schema_id, m.row.schema_version),
      });
      const flv = props.hub.flavorFor(m.row.schema_id, m.row.schema_version);
      if (flv !== null) flavors.add(flv);
    }
    for (const prov of graph.state().memoryProvenance.values()) {
      if (prov.authoring_personality_instance_id !== null) {
        authors.add(prov.authoring_personality_instance_id);
      }
    }
    return {
      flavors: Array.from(flavors).sort(),
      schemas: Array.from(schemas.values()).sort((a, b) => a.schemaId.localeCompare(b.schemaId)),
      authors: Array.from(authors).sort(),
    };
  });

  const selectedMemory = createMemo<DecodedMemory | null>(() => {
    const id = selectedId();
    if (id === null) return null;
    const fromMemories = graph.state().memoriesById.get(id);
    if (fromMemories !== undefined) return fromMemories;
    const goal = graph.state().goalsById.get(id);
    if (goal !== undefined) return goalToDecodedMemory(goal);
    return null;
  });
  const requestedPayloadIds = new Set<string>();

  createEffect(() => {
    const selected = selectedMemory();
    if (selected === null || requestedPayloadIds.has(selected.row.id)) return;
    if (selected.row.payload.length > 0) return;
    requestedPayloadIds.add(selected.row.id);
    if (selected.row.kind === "Goal") {
      void graph.hydrate?.({ goal_ids: [selected.row.id] });
    } else {
      void graph.hydrate?.({ memory_ids: [selected.row.id] });
    }
  });

  const lineage = createMemo(() => {
    const id = selectedId();
    if (id === null) return { outbound: [], inbound: [] };
    return oneHopLineage(id, graph.state().edgesById, graph.state().memoriesById);
  });

  const rowsForList = createMemo<DecodedMemory[]>(() => {
    const f = filtered();
    return [
      ...f.memories,
      ...f.goals.map(goalToDecodedMemory),
    ];
  });

  const events = createMemo(() =>
    Array.from(graph.state().eventsBySeq.values()).sort((a, b) =>
      a.seq < b.seq ? 1 : -1,
    ),
  );

  const proposals = createMemo<GoalRow[]>(() =>
    Array.from(graph.state().goalsById.values()).filter(
      (g) => g.state === "Proposed",
    ),
  );

  const declineProposal = async (proposal: GoalRow): Promise<void> => {
    if (decliningId() !== null) return;
    setDecliningId(proposal.id);
    setRequestsError(null);
    const draft: GoalDraft = {
      owner: proposal.owner,
      schema_id: proposal.schema_id,
      schema_version: proposal.schema_version,
      title: proposal.title,
      text: proposal.text,
      payload: proposal.payload,
      state: "Rejected",
      parent_goal_ids: proposal.parent_goal_ids,
      supersedes_goal_id: proposal.id,
      authorship: "User",
      request_id: `surface-decline:${proposal.id}:${Date.now()}`,
    };
    try {
      const result = await commands.goalWrite(draft);
      if (result.status === "error") {
        const raw = result.error as unknown;
        const message =
          typeof raw === "object" && raw !== null && "message" in raw
            ? String((raw as { message: unknown }).message)
            : typeof raw === "string"
              ? raw
              : "decline failed";
        setRequestsError(message);
        return;
      }
      void graph.refresh?.();
    } finally {
      setDecliningId(null);
    }
  };

  onMount(() => {
    const cleanup = installSurfaceKeys({
      onTab: setActiveTab,
      onToggleFilters: () => setDrawerOpen((o) => !o),
      onToggleEventStream: () => setEventsOpen((o) => !o),
      onCloseDrawer: () => { setDrawerOpen(false); setEventsOpen(false); },
    });
    onCleanup(cleanup);
  });

  return (
    <div class="surface">
      <TabStrip
        active={activeTab()}
        counts={counts()}
        onChange={setActiveTab}
        onToggleFilters={() => setDrawerOpen((o) => !o)}
      />
      <ChipRail flavors={facets().flavors} />
      <RequestsStrip
        proposals={proposals()}
        pendingId={decliningId()}
        onAccept={(proposal) => setAcceptProposal(proposal)}
        onDecline={(proposal) => {
          void declineProposal(proposal);
        }}
      />
      <Show when={requestsError() !== null}>
        <p class="surface-requests__error">{requestsError()}</p>
      </Show>
      <Show when={acceptProposal()} keyed>
        {(proposal) => (
          <GoalDialog
            hub={props.hub}
            proposal={proposal}
            onClose={() => setAcceptProposal(null)}
            onAfterWrite={() => {
              void graph.refresh?.();
            }}
          />
        )}
      </Show>
      <div class="surface__body">
        <RowList
          rows={rowsForList()}
          provenance={graph.state().memoryProvenance}
          activeTab={activeTab()}
          selectedId={selectedId()}
          onSelect={setSelectedId}
        />
        <Show when={selectedMemory()}>
          <DetailPane
            memory={selectedMemory()!}
            provenance={graph.state().memoryProvenance.get(selectedMemory()!.row.id)}
            lineage={lineage()}
            flavor={props.hub.flavorFor(selectedMemory()!.row.schema_id, selectedMemory()!.row.schema_version)}
            hub={props.hub}
          />
        </Show>
      </div>
      <ActivityStrip
        state={STATE_FROM_STREAM[graph.state().streamStatus] ?? "idle"}
        lastWakeAtMs={null} // v1: derived in follow-up
        activePersonalityCount={0} // v1: derived in follow-up
        onToggleEventStream={() => setEventsOpen((o) => !o)}
      />
      <FilterDrawer open={drawerOpen()} onClose={() => setDrawerOpen(false)} facets={facets()} />
      <Show when={eventsOpen()}>
        <EventStream
          collapsed={false}
          onToggle={() => setEventsOpen(false)}
          width={360}
          onResizeStart={() => {}}
          onResizeKeyDown={() => {}}
          events={events()}
          hub={props.hub}
        />
      </Show>
    </div>
  );
};
