import {
  For,
  Show,
  createMemo,
  createSignal,
  onCleanup,
  onMount,
  type Accessor,
  type Component,
} from "solid-js";
import type { PersonalityInstanceTs, WakeEntryDraftTs } from "../../bindings";
import { Mono } from "../../primitives";
import type {
  CanvasEdge,
  CanvasModel,
  CanvasNode,
  PersonalitySelection,
} from "./types";

interface CanvasProps {
  model: Accessor<CanvasModel | undefined>;
  selection: PersonalitySelection;
  drafts: Map<string, WakeEntryDraftTs[]>;
  onSelect: (selection: PersonalitySelection) => void;
}

const MIN_ZOOM = 0.4;
const MAX_ZOOM = 1.8;
const DEFAULT_ZOOM = 0.9;
const DEFAULT_PAN = { x: 40, y: 40 };

const clampZoom = (value: number): number =>
  Math.min(MAX_ZOOM, Math.max(MIN_ZOOM, value));

export const PersonalityCanvas: Component<CanvasProps> = (props) => {
  const [zoom, setZoom] = createSignal(DEFAULT_ZOOM);
  const [pan, setPan] = createSignal(DEFAULT_PAN);
  let containerRef: HTMLDivElement | undefined;
  let panState: { startX: number; startY: number; origin: { x: number; y: number } } | null = null;

  onMount(() => {
    const wheel = (event: WheelEvent) => {
      event.preventDefault();
      const factor = event.deltaY < 0 ? 1.08 : 1 / 1.08;
      setZoom((z) => clampZoom(z * factor));
    };
    containerRef?.addEventListener("wheel", wheel, { passive: false });
    onCleanup(() => containerRef?.removeEventListener("wheel", wheel));
  });

  const startPan = (event: PointerEvent) => {
    if (event.button !== 0) return;
    if (
      (event.target as HTMLElement).closest(
        "[data-canvas-node], [data-canvas-control]",
      )
    ) {
      return;
    }
    panState = {
      startX: event.clientX,
      startY: event.clientY,
      origin: pan(),
    };
    (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
    props.onSelect(null);
  };

  const movePan = (event: PointerEvent) => {
    if (!panState) return;
    const dx = event.clientX - panState.startX;
    const dy = event.clientY - panState.startY;
    setPan({ x: panState.origin.x + dx, y: panState.origin.y + dy });
  };

  const endPan = (event: PointerEvent) => {
    if (!panState) return;
    panState = null;
    (event.currentTarget as HTMLElement).releasePointerCapture(event.pointerId);
  };

  const transform = createMemo(
    () => `translate(${pan().x}px, ${pan().y}px) scale(${zoom()})`,
  );

  return (
    <div
      ref={containerRef}
      class="personality-canvas"
      onPointerDown={startPan}
      onPointerMove={movePan}
      onPointerUp={endPan}
      onPointerCancel={endPan}
    >
      <Show
        when={props.model() && props.model()!.nodes.length > 0}
        fallback={
          <div class="personality-canvas-empty">
            <p>No personalities yet.</p>
          </div>
        }
      >
        <div
          class="personality-canvas-stage"
          data-testid="personality-canvas-stage"
          style={{ transform: transform() }}
        >
          <svg
            class="personality-canvas-edges"
            width={props.model()?.width ?? 0}
            height={props.model()?.height ?? 0}
          >
            <defs>
              <marker
                id="personality-arrow"
                viewBox="0 0 10 10"
                refX="9"
                refY="5"
                markerWidth="8"
                markerHeight="8"
                orient="auto-start-reverse"
              >
                <path d="M 0 0 L 10 5 L 0 10 z" fill="currentColor" />
              </marker>
            </defs>
            <For each={props.model()?.edges ?? []}>
              {(edge) => (
                <EdgeShape
                  edge={edge}
                  selected={isEdgeSelected(props.selection, edge)}
                  onSelect={() => {
                    if (edge.kind !== "trigger") return;
                    props.onSelect({
                      kind: "edge",
                      schema_id: edge.shape_id,
                      tgt_instance_id: edge.tgt_instance_id,
                      tgt_entry_index: edge.tgt_entry_index,
                    });
                  }}
                />
              )}
            </For>
          </svg>
          <For each={props.model()?.nodes ?? []}>
            {(node) => (
              <NodeShell
                node={node}
                selection={props.selection}
                drafts={props.drafts}
                onSelect={props.onSelect}
              />
            )}
          </For>
        </div>
      </Show>
      <CanvasOverlay zoom={zoom} setZoom={setZoom} resetView={() => {
        setZoom(DEFAULT_ZOOM);
        setPan(DEFAULT_PAN);
      }} />
    </div>
  );
};

const isEdgeSelected = (
  selection: PersonalitySelection,
  edge: CanvasEdge,
): boolean =>
  edge.kind === "trigger" &&
  selection?.kind === "edge" &&
  selection.schema_id === edge.shape_id &&
  selection.tgt_instance_id === edge.tgt_instance_id &&
  selection.tgt_entry_index === edge.tgt_entry_index;

const EdgeShape: Component<{
  edge: CanvasEdge;
  selected: boolean;
  onSelect: () => void;
}> = (props) => {
  const isProduces = () => props.edge.kind === "produces";
  return (
    <g
      class={`personality-edge${props.selected ? " is-selected" : ""}${
        isProduces() ? " is-produces" : ""
      }`}
      onClick={(event) => {
        if (isProduces()) return; // produces edges are non-selectable in v1
        event.stopPropagation();
        props.onSelect();
      }}
    >
      <path
        d={props.edge.path}
        class="personality-edge-line"
        marker-end="url(#personality-arrow)"
      />
      <path d={props.edge.path} class="personality-edge-hit" />
    </g>
  );
};

const NodeShell: Component<{
  node: CanvasNode;
  selection: PersonalitySelection;
  drafts: Map<string, WakeEntryDraftTs[]>;
  onSelect: (selection: PersonalitySelection) => void;
}> = (props) => {
  const style = () => ({
    left: `${props.node.x}px`,
    top: `${props.node.y}px`,
    width: `${props.node.width}px`,
    "min-height": `${props.node.height}px`,
  });

  if (props.node.kind === "schema") {
    return (
      <div
        class="personality-canvas-schema"
        style={style()}
        data-canvas-node="schema"
      >
        <Mono>{(props.node.data as { schema_id: string }).schema_id}</Mono>
      </div>
    );
  }
  if (props.node.kind === "relation") {
    return (
      <div
        class="personality-canvas-relation"
        style={style()}
        data-canvas-node="relation"
      >
        <Mono>{(props.node.data as { relation_id: string }).relation_id}</Mono>
      </div>
    );
  }
  return (
    <PersonalityCardNode
      node={props.node}
      selection={props.selection}
      drafts={props.drafts}
      onSelect={props.onSelect}
    />
  );
};

const PersonalityCardNode: Component<{
  node: CanvasNode;
  selection: PersonalitySelection;
  drafts: Map<string, WakeEntryDraftTs[]>;
  onSelect: (selection: PersonalitySelection) => void;
}> = (props) => {
  const data = () =>
    props.node.data as { kind: "personality"; instance: PersonalityInstanceTs };
  const instance = () => data().instance;
  const drafts = () =>
    props.drafts.get(instance().personality_instance_id) ??
    instance().wake_entries;
  const isSelected = () =>
    props.selection?.kind === "personality" &&
    props.selection.instance_id === instance().personality_instance_id;
  const selectedEntryIndex = () =>
    props.selection?.kind === "wake_entry" &&
    props.selection.instance_id === instance().personality_instance_id
      ? props.selection.entry_index
      : -1;

  return (
    <article
      class={`personality-canvas-node${isSelected() ? " is-selected" : ""}`}
      data-canvas-node="personality"
      data-testid="personality-card"
      style={{
        left: `${props.node.x}px`,
        top: `${props.node.y}px`,
        width: `${props.node.width}px`,
        "min-height": `${props.node.height}px`,
      }}
      onClick={(event) => {
        event.stopPropagation();
        props.onSelect({
          kind: "personality",
          instance_id: instance().personality_instance_id,
        });
      }}
    >
      <header class="personality-canvas-node-head">
        <strong>{instance().display_name}</strong>
        <span class={`personality-status ${instance().status}`}>
          {instance().status}
        </span>
      </header>
      <div class="personality-canvas-node-meta">
        <span class="personality-flavor-chip" data-testid="personality-flavor-chip">
          Instance
        </span>
        <Mono>{shortId(instance().personality_instance_id)}</Mono>
      </div>
      <ul class="personality-canvas-entries">
        <For each={drafts()}>
          {(entry, index) => (
            <li>
              <button
                type="button"
                class={`personality-canvas-entry${
                  selectedEntryIndex() === index() ? " is-selected" : ""
                }${entry.enabled ? "" : " is-disabled"}`}
                onClick={(event) => {
                  event.stopPropagation();
                  props.onSelect({
                    kind: "wake_entry",
                    instance_id: instance().personality_instance_id,
                    entry_index: index(),
                  });
                }}
              >
                <span class="personality-canvas-entry-label">
                  {entry.label || `entry ${index() + 1}`}
                </span>
                <Mono>{entry.trigger_id || "(no trigger)"}</Mono>
              </button>
            </li>
          )}
        </For>
        <Show when={drafts().length === 0}>
          <li class="personality-canvas-entries-empty">No wake entries.</li>
        </Show>
      </ul>
    </article>
  );
};

const CanvasOverlay: Component<{
  zoom: Accessor<number>;
  setZoom: (z: number) => void;
  resetView: () => void;
}> = (props) => (
  <div class="personality-canvas-overlay" data-canvas-control="zoom">
    <button
      type="button"
      class="hub-nav-item"
      aria-label="Zoom in"
      onClick={() => props.setZoom(clampZoom(props.zoom() * 1.12))}
    >
      +
    </button>
    <button
      type="button"
      class="hub-nav-item"
      aria-label="Zoom out"
      onClick={() => props.setZoom(clampZoom(props.zoom() / 1.12))}
    >
      −
    </button>
    <button
      type="button"
      class="hub-nav-item"
      aria-label="Reset zoom"
      onClick={props.resetView}
    >
      Reset
    </button>
    <span class="personality-canvas-zoom-readout">
      {Math.round(props.zoom() * 100)}%
    </span>
  </div>
);

const shortId = (value: string): string => value.slice(0, 8);
