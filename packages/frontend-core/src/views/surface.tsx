import "./surface.css";
import { Show, createMemo, createSignal, onCleanup, onMount, type Component } from "solid-js";
import { useGraph } from "../graph-store";
import { useGraphFilter } from "../graph-filter-store";
import { filterGraphSnapshot, oneHopLineage } from "../graph-selectors";
import type { Hub } from "../hub";
import type { GoalRow } from "../bindings";
import type { DecodedMemory } from "../graph-store";
import { ActivityStrip, type EngineState } from "./surface/activity-strip";
import { ChipRail } from "./surface/chip-rail";
import { DetailPane } from "./surface/detail-pane";
import { FilterDrawer, type FilterFacets } from "./surface/filter-drawer";
import { RowList, type ActiveTab } from "./surface/row-list";
import { TabStrip } from "./surface/tab-strip";
import { installSurfaceKeys } from "./surface/keys";
import { EventStream } from "./surface-events";

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
