import {
  For,
  Show,
  createEffect,
  createMemo,
  createSignal,
  type Accessor,
  type Component,
  type JSX,
} from "solid-js";
import type { Hub, Renderer } from "../../hub";
import type { Adjacency, AtlasNode, InEntry, OutEntry } from "./types";
import { KIND_GLYPH, LAYER_Z, TINT_HEX } from "./three-helpers";

type InspectorTab = "payload" | "edges" | "meta" | "raw";

const tabs: Array<{ id: InspectorTab; label: string }> = [
  { id: "payload", label: "Payload" },
  { id: "edges", label: "Edges" },
  { id: "meta", label: "Meta" },
  { id: "raw", label: "Raw" },
];

const relationLabel = (kind: string): string => {
  const relationParts = kind.split("--");
  const relationTail = relationParts[relationParts.length - 1] ?? kind;
  const pathParts = relationTail.split("/");
  const tail = pathParts[pathParts.length - 1] ?? relationTail;
  return tail.replace(/[_-]+/g, " ").trim() || kind;
};

const classToken = (value: string | undefined): string =>
  value?.toLowerCase().replace(/[^a-z0-9_-]+/g, "-") ?? "unknown";

const nodeLabel = (node: AtlasNode | undefined): string =>
  node?.title?.trim() || nodeSchemaLabel(node);

const nodeSchemaLabel = (node: AtlasNode | undefined): string =>
  node ? `${node.schemaId} @ v${node.schemaVersion}` : "filtered node";

const nodeKindLabel = (node: AtlasNode | undefined): string =>
  node ? `${node.kind}${node.flavor ? ` · ƒ:${node.flavor}` : ""}` : "not visible";

const ownerLabel = (node: AtlasNode): string => {
  const owner = node.memory?.owner ?? node.goal?.owner;
  if (owner === undefined) return "unknown";
  const principal =
    owner.principal.User !== undefined
      ? `User:${owner.principal.User}`
      : `Group:${owner.principal.Group}`;
  return `${principal} · org:${owner.org_id}`;
};

const payloadBytes = (node: AtlasNode): string => {
  const bytes = node.memory?.payload ?? node.goal?.payload;
  return bytes === undefined ? "unknown" : `${bytes.length}`;
};

const normalizeForJson = (value: unknown): unknown => {
  if (value instanceof Uint8Array) return Array.from(value);
  if (Array.isArray(value)) return value.map(normalizeForJson);
  if (value !== null && typeof value === "object") {
    return Object.fromEntries(
      Object.entries(value as Record<string, unknown>).map(([k, v]) => [
        k,
        normalizeForJson(v),
      ]),
    );
  }
  return value;
};

const jsonBlock = (value: unknown): string => {
  const text = JSON.stringify(normalizeForJson(value), null, 2) ?? "null";
  return text.length > 12_000 ? `${text.slice(0, 12_000)}\n... truncated` : text;
};

const rawNode = (node: AtlasNode): Record<string, unknown> => ({
  id: node.id,
  kind: node.kind,
  schemaId: node.schemaId,
  schemaVersion: node.schemaVersion,
  flavor: node.flavor,
  title: node.title,
  x: node.x,
  y: node.y,
  z: LAYER_Z[node.kind],
  owner: node.memory?.owner ?? node.goal?.owner,
  row: node.memory ?? node.goal ?? null,
  payload: node.payload ?? null,
  decodeError: node.decodeError ?? null,
});

const Field: Component<{ label: string; children: JSX.Element }> = (props) => (
  <div class="i-row">
    <span class="k">{props.label}</span>
    <span class="v">{props.children}</span>
  </div>
);

const TabButton: Component<{
  active: boolean;
  onClick: () => void;
  children: JSX.Element;
}> = (props) => (
  <button
    type="button"
    class={`i-tab ${props.active ? "on" : ""}`}
    onClick={props.onClick}
  >
    {props.children}
  </button>
);

const PayloadPanel: Component<{
  node: AtlasNode;
  renderer: Renderer<unknown> | null;
}> = (props) => (
  <div class="i-section">
    <Show when={props.node.decodeError}>
      {(err) => (
        <div class="i-error">
          <div class="i-error-head">{err().kind}</div>
          <div>{err().message}</div>
        </div>
      )}
    </Show>

    <Show when={props.node.goal}>
      {(goal) => (
        <div class="i-goal-payload">
          <div class="i-goal-text">{goal().text}</div>
          <div class="i-meta compact">
            <Field label="state">{goal().state}</Field>
            <Field label="parents">{goal().parent_goal_ids.length}</Field>
            <Field label="supersedes">{goal().supersedes ?? "none"}</Field>
          </div>
        </div>
      )}
    </Show>

    <Show
      when={props.node.memory}
      fallback={
        <Show when={!props.node.goal}>
          <div class="i-empty-state">No backing row is available for this node.</div>
        </Show>
      }
    >
      {(memory) => {
        const renderer = props.renderer;
        if (renderer !== null && props.node.decodeError === undefined) {
          return (
            <div class="i-payload-renderer">
              {renderer.render({
                memory: memory(),
                payload: props.node.payload,
              })}
            </div>
          );
        }
        if (props.node.payload !== undefined && props.node.payload !== null) {
          return <pre class="i-json">{jsonBlock(props.node.payload)}</pre>;
        }
        return (
          <div class="i-empty-state">
            Payload unavailable. The row has no decoded sidecar data in the
            current snapshot.
          </div>
        );
      }}
    </Show>
  </div>
);

const MetaPanel: Component<{
  node: AtlasNode;
  rendererFlavor: string | null;
  hasRenderer: boolean;
}> = (props) => (
  <div class="i-meta">
    <Field label="id">
      <span class="mono">{props.node.id}</span>
    </Field>
    <Field label="schema">
      <span class="mono">
        {props.node.schemaId} @ v{props.node.schemaVersion}
      </span>
    </Field>
    <Field label="renderer">
      <Show
        when={props.hasRenderer}
        fallback={<em>(none registered - substrate default)</em>}
      >
        via ƒ:{props.rendererFlavor}
      </Show>
    </Field>
    <Field label="payload bytes">
      <span class="mono">{payloadBytes(props.node)}</span>
    </Field>
    <Field label="owner">
      <span class="mono">{ownerLabel(props.node)}</span>
    </Field>
    <Field label="x, y">
      <span class="mono">
        {props.node.x.toFixed(2)}, {props.node.y.toFixed(2)}
      </span>
    </Field>
    <Field label="layer z">
      <span class="mono">{LAYER_Z[props.node.kind]}</span>
    </Field>
  </div>
);

const EdgeRow: Component<{
  direction: "incoming" | "outgoing";
  edge: InEntry | OutEntry;
  peer: AtlasNode | undefined;
  onPick: () => void;
}> = (props) => {
  const edge = () => props.edge;
  const relationClass = () => edge().relationClass ?? "Relation";
  return (
    <button
      type="button"
      class="i-edge"
      onClick={props.onPick}
      title={`${edge().kind} ${props.direction === "incoming" ? "<-" : "->"} ${nodeLabel(props.peer)}`}
    >
      <span class={`i-edge-cls ${classToken(edge().relationClass)}`}>
        {relationClass()}
      </span>
      <span class="i-edge-main">
        <span class="i-edge-rel">{relationLabel(edge().kind)}</span>
        <span class="i-edge-tgt">{nodeLabel(props.peer)}</span>
      </span>
      <span class="i-edge-node">{nodeKindLabel(props.peer)}</span>
    </button>
  );
};

const EdgesPanel: Component<{
  out: OutEntry[];
  inn: InEntry[];
  byId: Map<string, AtlasNode>;
  onPickNode: (id: string) => void;
}> = (props) => (
  <div class="i-edges">
    <div class="i-edges-head">outgoing ({props.out.length})</div>
    <Show
      when={props.out.length > 0}
      fallback={<div class="i-empty-state">No outgoing edges.</div>}
    >
      <For each={props.out.slice(0, 20)}>
        {(e) => (
          <EdgeRow
            direction="outgoing"
            edge={e}
            peer={props.byId.get(e.tgt)}
            onPick={() => props.onPickNode(e.tgt)}
          />
        )}
      </For>
    </Show>

    <div class="i-edges-head spaced">incoming ({props.inn.length})</div>
    <Show
      when={props.inn.length > 0}
      fallback={<div class="i-empty-state">No incoming edges.</div>}
    >
      <For each={props.inn.slice(0, 20)}>
        {(e) => (
          <EdgeRow
            direction="incoming"
            edge={e}
            peer={props.byId.get(e.src)}
            onPick={() => props.onPickNode(e.src)}
          />
        )}
      </For>
    </Show>
  </div>
);

// ── Filter Pill primitive ──────────────────────────────────────────────
export const Pill: Component<{
  active: boolean;
  onClick: () => void;
  color: string;
  count?: number;
  children: JSX.Element;
}> = (props) => (
  <button
    type="button"
    class={`atlas-pill ${props.active ? "on" : "off"}`}
    style={{ "--pill-color": props.color }}
    onClick={props.onClick}
  >
    <span class="dot" />
    <span class="lbl">{props.children}</span>
    <Show when={props.count != null}>
      <span class="ct">{props.count}</span>
    </Show>
  </button>
);

// ── Inspector (right panel) ────────────────────────────────────────────
export const Inspector: Component<{
  hub: Hub;
  node: AtlasNode | null;
  adj: Adjacency;
  byId: Map<string, AtlasNode>;
  onPickNode: (id: string) => void;
}> = (props) => (
  <Show
    when={props.node}
    fallback={
      <div class="atlas-inspector empty">
        <div class="inspector-empty-head">Atlas inspector</div>
        <div class="inspector-empty-body">
          Click a node to open. Hover to preview. Click an outgoing or
          incoming edge to walk the chain.
        </div>
        <div class="inspector-legend">
          <div class="leg-row">
            <span style={{ color: TINT_HEX.Fact }}>{KIND_GLYPH.Fact}</span>{" "}
            Fact <em>z=0</em>
          </div>
          <div class="leg-row">
            <span style={{ color: TINT_HEX.Abstraction }}>
              {KIND_GLYPH.Abstraction}
            </span>{" "}
            Abstraction <em>z=1.6</em>
          </div>
          <div class="leg-row">
            <span style={{ color: TINT_HEX.Perspective }}>
              {KIND_GLYPH.Perspective}
            </span>{" "}
            Perspective <em>z=3.2</em>
          </div>
          <div class="leg-row">
            <span style={{ color: TINT_HEX.Goal }}>{KIND_GLYPH.Goal}</span>{" "}
            Goal <em>z=4.8</em>
          </div>
          <div class="leg-rule" />
          <div class="leg-row faint">edges uniform · click to walk chain</div>
        </div>
      </div>
    }
  >
    {(node: Accessor<AtlasNode>) => {
      const [tab, setTab] = createSignal<InspectorTab>("payload");
      const out = () => props.adj.out.get(node().id) ?? [];
      const inn = () => props.adj.inn.get(node().id) ?? [];
      const hasPayloadDetail = () =>
        node().memory !== undefined ||
        node().goal !== undefined ||
        node().payload !== undefined ||
        node().decodeError !== undefined;
      createEffect(() => {
        node().id;
        setTab(
          hasPayloadDetail() || out().length + inn().length === 0
            ? "payload"
            : "edges",
        );
      });
      const renderer = createMemo(() =>
        props.hub.rendererFor(node().schemaId, node().schemaVersion),
      );
      const rendererFlavor = createMemo(() => {
        const r = props.hub.registeredRenderers().find(
          (rr) =>
            rr.schemaId === node().schemaId &&
            rr.schemaVersion === node().schemaVersion,
        );
        return r?.flavor ?? null;
      });
      return (
        <div class="atlas-inspector">
          <div class="i-head">
            <span class="i-glyph" style={{ color: TINT_HEX[node().kind] }}>
              {KIND_GLYPH[node().kind]}
            </span>
            <span class="i-kind">{node().kind}</span>
            <Show when={node().flavor}>
              <span class="i-flavor">ƒ:{node().flavor}</span>
            </Show>
          </div>

          <div class="i-title">{nodeLabel(node())}</div>
          <div class="i-schema">
            {node().schemaId} @ v{node().schemaVersion}
          </div>

          <div class="i-tabs" role="tablist" aria-label="Inspector sections">
            <For each={tabs}>
              {(entry) => (
                <TabButton
                  active={tab() === entry.id}
                  onClick={() => setTab(entry.id)}
                >
                  {entry.label}
                </TabButton>
              )}
            </For>
          </div>

          <Show when={tab() === "payload"}>
            <PayloadPanel node={node()} renderer={renderer()} />
          </Show>
          <Show when={tab() === "edges"}>
            <EdgesPanel
              out={out()}
              inn={inn()}
              byId={props.byId}
              onPickNode={props.onPickNode}
            />
          </Show>
          <Show when={tab() === "meta"}>
            <MetaPanel
              node={node()}
              rendererFlavor={rendererFlavor()}
              hasRenderer={renderer() !== null}
            />
          </Show>
          <Show when={tab() === "raw"}>
            <pre class="i-json">{jsonBlock(rawNode(node()))}</pre>
          </Show>
        </div>
      );
    }}
  </Show>
);
