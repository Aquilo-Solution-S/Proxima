import { For, Show, type Component, type JSX } from "solid-js";
import type { Hub } from "../../hub";
import type { Adjacency, AtlasNode } from "./types";
import { KIND_GLYPH, LAYER_Z, TINT_HEX } from "./three-helpers";

const relationLabel = (kind: string): string => {
  const relationParts = kind.split("--");
  const relationTail = relationParts[relationParts.length - 1] ?? kind;
  const pathParts = relationTail.split("/");
  const tail = pathParts[pathParts.length - 1] ?? relationTail;
  return tail.replace(/[_-]+/g, " ").trim() || kind;
};

const nodeLabel = (node: AtlasNode | undefined): string =>
  node?.title?.trim() || nodeSchemaLabel(node);

const nodeSchemaLabel = (node: AtlasNode | undefined): string =>
  node ? `${node.schemaId} @ v${node.schemaVersion}` : "filtered node";

const nodeKindLabel = (node: AtlasNode | undefined): string =>
  node ? `${node.kind}${node.flavor ? ` · ƒ:${node.flavor}` : ""}` : "not visible";

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
    {(node) => {
      const out = () => props.adj.out.get(node().id) ?? [];
      const inn = () => props.adj.inn.get(node().id) ?? [];
      const renderer = () =>
        props.hub.rendererFor(node().schemaId, node().schemaVersion);
      const rendererFlavor = () => {
        const r = props.hub.registeredRenderers().find(
          (rr) =>
            rr.schemaId === node().schemaId &&
            rr.schemaVersion === node().schemaVersion,
        );
        return r?.flavor ?? null;
      };
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
          <Show when={node().title}>
            <div class="i-title">{node().title}</div>
          </Show>
          <div class="i-schema">
            {node().schemaId} @ v{node().schemaVersion}
          </div>

          <div class="i-meta">
            <div class="i-row">
              <span class="k">renderer</span>
              <span class="v">
                <Show
                  when={renderer()}
                  fallback={<em>(none registered — substrate default)</em>}
                >
                  via ƒ:{rendererFlavor()} (payload pending data wiring)
                </Show>
              </span>
            </div>
            <div class="i-row">
              <span class="k">x, y</span>
              <span class="v mono">
                {node().x.toFixed(2)}, {node().y.toFixed(2)}
              </span>
            </div>
            <div class="i-row">
              <span class="k">layer z</span>
              <span class="v mono">{LAYER_Z[node().kind]}</span>
            </div>
          </div>

          <Show when={out().length > 0}>
            <div class="i-edges">
              <div class="i-edges-head">→ outgoing ({out().length})</div>
              <For each={out().slice(0, 10)}>
                {(e) => {
                  const t = props.byId.get(e.tgt);
                  return (
                    <div
                      class="i-edge"
                      onClick={() => props.onPickNode(e.tgt)}
                      title={`${e.kind} -> ${nodeLabel(t)}`}
                    >
                      <span class="i-edge-tgt">{nodeLabel(t)}</span>
                      <span class="i-edge-meta">
                        <span class="i-edge-cls">{relationLabel(e.kind)}</span>
                        <span class="i-edge-node">{nodeKindLabel(t)}</span>
                      </span>
                    </div>
                  );
                }}
              </For>
            </div>
          </Show>

          <Show when={inn().length > 0}>
            <div class="i-edges">
              <div class="i-edges-head">← incoming ({inn().length})</div>
              <For each={inn().slice(0, 10)}>
                {(e) => {
                  const s = props.byId.get(e.src);
                  return (
                    <div
                      class="i-edge"
                      onClick={() => props.onPickNode(e.src)}
                      title={`${e.kind} <- ${nodeLabel(s)}`}
                    >
                      <span class="i-edge-tgt">{nodeLabel(s)}</span>
                      <span class="i-edge-meta">
                        <span class="i-edge-cls">{relationLabel(e.kind)}</span>
                        <span class="i-edge-node">{nodeKindLabel(s)}</span>
                      </span>
                    </div>
                  );
                }}
              </For>
            </div>
          </Show>
        </div>
      );
    }}
  </Show>
);
