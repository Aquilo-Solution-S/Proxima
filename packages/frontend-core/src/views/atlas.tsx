import "./atlas.css";
/*
 * PLACEHOLDER DATA STATE
 * This component renders the substrate Atlas — three.js scene with locked
 * z-by-layer (F=0, A=1.6, P=3.2, G=4.8), camera, orbit, picking, and
 * uniform-grey edges. nodes/edges default to []; filter rail and Inspector
 * panel are 2e.3, real data wiring (commands.atlas) is a follow-up that
 * needs backend embedding positions first.
 */

import {
  For,
  Show,
  type Component,
  type JSX,
  createEffect,
  createMemo,
  createSignal,
  onCleanup,
  onMount,
} from "solid-js";
import * as THREE from "three";
import type { Hub } from "../hub";

// ── Substrate types ─────────────────────────────────────────────────────
export type AtlasNodeKind = "Fact" | "Abstraction" | "Perspective" | "Goal";

export interface AtlasNode {
  id: string;
  kind: AtlasNodeKind;
  schemaId: string;
  schemaVersion: number;
  flavor: string | null;
  x: number; // embedding x (sticky)
  y: number; // embedding y (sticky)
  title?: string;
}

export interface AtlasEdge {
  id: string;
  src: string;
  tgt: string;
  kind: string;
}

// ── Layer + tint discipline ────────────────────────────────────────────
const LAYER_Z: Record<AtlasNodeKind, number> = {
  Fact: 0,
  Abstraction: 1.6,
  Perspective: 3.2,
  Goal: 4.8,
};

const TINT: Record<AtlasNodeKind, number> = {
  Fact: 0xa8aeba,
  Abstraction: 0xc9a86a,
  Perspective: 0xe8e4d6,
  Goal: 0xd9c28a,
};

const TINT_HEX: Record<AtlasNodeKind, string> = {
  Fact: "#A8AEBA",
  Abstraction: "#C9A86A",
  Perspective: "#E8E4D6",
  Goal: "#D9C28A",
};

const KIND_GLYPH: Record<AtlasNodeKind, string> = {
  Fact: "◆",
  Abstraction: "△",
  Perspective: "▽",
  Goal: "◇",
};

const LAYER_LABELS: Array<{ z: number; t: string; c: string }> = [
  { z: 0, t: "F · Facts", c: TINT_HEX.Fact },
  { z: 1.6, t: "A · Abstractions", c: TINT_HEX.Abstraction },
  { z: 3.2, t: "P · Perspectives", c: TINT_HEX.Perspective },
  { z: 4.8, t: "G · Goals", c: TINT_HEX.Goal },
];

// ── Adjacency + chain traversal ────────────────────────────────────────
interface OutEntry {
  tgt: string;
  id: string;
  kind: string;
}
interface InEntry {
  src: string;
  id: string;
  kind: string;
}
interface Adjacency {
  out: Map<string, OutEntry[]>;
  inn: Map<string, InEntry[]>;
}

function buildAdjacency(edges: AtlasEdge[]): Adjacency {
  const out = new Map<string, OutEntry[]>();
  const inn = new Map<string, InEntry[]>();
  for (const e of edges) {
    if (!out.has(e.src)) out.set(e.src, []);
    if (!inn.has(e.tgt)) inn.set(e.tgt, []);
    out.get(e.src)!.push({ tgt: e.tgt, id: e.id, kind: e.kind });
    inn.get(e.tgt)!.push({ src: e.src, id: e.id, kind: e.kind });
  }
  return { out, inn };
}

interface Chain {
  nodes: Set<string>;
  edges: Set<string>;
}

function chainOf(nodeId: string, adj: Adjacency, depth = 5): Chain {
  const nodes = new Set<string>([nodeId]);
  const edges = new Set<string>();
  const stack: Array<[string, number]> = [[nodeId, 0]];
  while (stack.length) {
    const [id, d] = stack.pop()!;
    if (d >= depth) continue;
    for (const { tgt, id: eid } of adj.out.get(id) ?? []) {
      edges.add(eid);
      if (!nodes.has(tgt)) {
        nodes.add(tgt);
        stack.push([tgt, d + 1]);
      }
    }
  }
  const stack2: Array<[string, number]> = [[nodeId, 0]];
  while (stack2.length) {
    const [id, d] = stack2.pop()!;
    if (d >= depth) continue;
    for (const { src, id: eid } of adj.inn.get(id) ?? []) {
      edges.add(eid);
      if (!nodes.has(src)) {
        nodes.add(src);
        stack2.push([src, d + 1]);
      }
    }
  }
  return { nodes, edges };
}

// ── Geometry per kind ──────────────────────────────────────────────────
function geometryFor(kind: AtlasNodeKind): THREE.BufferGeometry {
  switch (kind) {
    case "Fact":
      return new THREE.SphereGeometry(0.085, 14, 10);
    case "Abstraction":
      return new THREE.OctahedronGeometry(0.13, 0);
    case "Perspective":
      return new THREE.TetrahedronGeometry(0.18, 0);
    case "Goal":
      return new THREE.OctahedronGeometry(0.16, 1);
  }
}

// ── Filter Pill primitive ──────────────────────────────────────────────
const Pill: Component<{
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
const Inspector: Component<{
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
          <div class="i-id">{node().id}</div>
          <div class="i-schema">
            {node().schemaId} @ v{node().schemaVersion}
          </div>
          <Show when={node().title}>
            <div class="i-title">{node().title}</div>
          </Show>

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
                    >
                      <span class="i-edge-cls">{e.kind}</span>
                      <span class="i-edge-tgt">{t?.title ?? e.tgt}</span>
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
                    >
                      <span class="i-edge-cls">{e.kind}</span>
                      <span class="i-edge-tgt">{s?.title ?? e.src}</span>
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

// ── Sprite labels for layers ───────────────────────────────────────────
function makeLayerLabel(text: string, color: string): THREE.Sprite {
  const c = document.createElement("canvas");
  c.width = 320;
  c.height = 76;
  const ctx = c.getContext("2d")!;
  ctx.font = "700 30px 'JetBrains Mono', monospace";
  ctx.textBaseline = "middle";
  ctx.lineWidth = 5;
  ctx.strokeStyle = "rgba(14, 16, 19, 0.86)";
  ctx.strokeText(text, 10, 38);
  ctx.fillStyle = color;
  ctx.fillText(text, 10, 38);
  const tex = new THREE.CanvasTexture(c);
  tex.minFilter = THREE.LinearFilter;
  const mat = new THREE.SpriteMaterial({ map: tex, transparent: true, opacity: 0.96 });
  const sp = new THREE.Sprite(mat);
  sp.scale.set(3.5, 0.84, 1);
  return sp;
}

// ── The Atlas substrate component ──────────────────────────────────────
export const Atlas: Component<{
  hub: Hub;
  nodes?: AtlasNode[];
  edges?: AtlasEdge[];
}> = (props) => {
  const nodes = () => props.nodes ?? [];
  const edges = () => props.edges ?? [];

  const [hoverId, setHoverId] = createSignal<string | null>(null);
  const [pickedId, setPickedId] = createSignal<string | null>(null);

  // Filters
  const [showFact, setShowFact] = createSignal(true);
  const [showAbs, setShowAbs] = createSignal(true);
  const [showPersp, setShowPersp] = createSignal(true);
  const [showGoal, setShowGoal] = createSignal(true);
  const [hiddenFlavors, setHiddenFlavors] = createSignal<Set<string>>(new Set());

  function toggleFlavor(f: string) {
    setHiddenFlavors((prev) => {
      const next = new Set(prev);
      if (next.has(f)) next.delete(f);
      else next.add(f);
      return next;
    });
  }

  const passKind = (k: AtlasNodeKind) =>
    (k === "Fact" && showFact()) ||
    (k === "Abstraction" && showAbs()) ||
    (k === "Perspective" && showPersp()) ||
    (k === "Goal" && showGoal());

  const passFlavor = (f: string | null) =>
    f === null || !hiddenFlavors().has(f);

  const byId = createMemo(() => new Map(nodes().map((n) => [n.id, n] as const)));
  const pickedNode = () => {
    const id = pickedId();
    return id ? byId().get(id) ?? null : null;
  };
  const hoverNode = () => {
    const id = hoverId();
    return id ? byId().get(id) ?? null : null;
  };
  const focusNode = () => hoverNode() ?? pickedNode();

  const counts = createMemo(() => {
    const kind: Record<AtlasNodeKind, number> = {
      Fact: 0,
      Abstraction: 0,
      Perspective: 0,
      Goal: 0,
    };
    const flavor: Record<string, number> = {};
    for (const n of nodes()) {
      kind[n.kind]++;
      if (n.flavor) flavor[n.flavor] = (flavor[n.flavor] ?? 0) + 1;
    }
    return { kind, flavor };
  });

  let mountRef!: HTMLDivElement;

  // Scene-level mutable handles, populated in onMount.
  const scene = new THREE.Scene();
  const renderer = new THREE.WebGLRenderer({ antialias: true, alpha: false });
  const camera = new THREE.PerspectiveCamera(38, 1, 0.1, 100);
  const nodeGroup = new THREE.Group();
  const haloGroup = new THREE.Group();
  const edgeGroup = new THREE.Group();
  const nodeMeshes = new Map<string, THREE.Mesh>();
  const haloMeshes = new Map<string, THREE.Mesh>();
  const edgeLines = new Map<string, THREE.Line>();

  const adj = createMemo(() => buildAdjacency(edges()));
  const focusId = () => hoverId() ?? pickedId();
  const chain = createMemo<Chain>(() => {
    const id = focusId();
    if (!id) return { nodes: new Set(), edges: new Set() };
    return chainOf(id, adj(), 5);
  });

  onMount(() => {
    scene.background = new THREE.Color(0x101319);
    scene.fog = new THREE.Fog(0x101319, 18, 42);

    const W = mountRef.clientWidth;
    const H = mountRef.clientHeight;
    camera.aspect = W / H;
    camera.updateProjectionMatrix();
    camera.position.set(11, 9, 14);
    camera.lookAt(0, 2.4, 0);

    renderer.setPixelRatio(Math.min(window.devicePixelRatio, 2));
    renderer.setSize(W, H);
    mountRef.appendChild(renderer.domElement);

    // Layer planes (faint horizontal grids at each z)
    for (const z of [0, 1.6, 3.2, 4.8]) {
      const grid = new THREE.GridHelper(20, 20, 0x3b4350, 0x242a33);
      grid.position.y = z;
      const mat = grid.material as THREE.Material;
      mat.transparent = true;
      mat.opacity = 0.55;
      scene.add(grid);
    }

    // Layer labels
    for (const { z, t, c } of LAYER_LABELS) {
      const sp = makeLayerLabel(t, c);
      sp.position.set(-9.6, z, -5.6);
      scene.add(sp);
    }

    scene.add(nodeGroup);
    scene.add(haloGroup);
    scene.add(edgeGroup);

    // ── Picking ──────────────────────────────────────────────────────
    const raycaster = new THREE.Raycaster();
    const pointer = new THREE.Vector2();
    let lastHover: string | null = null;

    function pickFrom(ev: PointerEvent): string | null {
      const rect = renderer.domElement.getBoundingClientRect();
      pointer.x = ((ev.clientX - rect.left) / rect.width) * 2 - 1;
      pointer.y = -((ev.clientY - rect.top) / rect.height) * 2 + 1;
      raycaster.setFromCamera(pointer, camera);
      const hits = raycaster.intersectObjects(nodeGroup.children, false);
      return (hits[0]?.object.userData.id as string | undefined) ?? null;
    }

    function onPointerMove(ev: PointerEvent) {
      const id = pickFrom(ev);
      if (id !== lastHover) {
        lastHover = id;
        setHoverId(id);
      }
      idleSince = performance.now();
    }
    function onClick(ev: MouseEvent) {
      const id = pickFrom(ev as unknown as PointerEvent);
      if (id) setPickedId(id);
    }
    renderer.domElement.addEventListener("pointermove", onPointerMove);
    renderer.domElement.addEventListener("click", onClick);

    // ── Hand-rolled orbit (no OrbitControls dep) ─────────────────────
    let dragging = false;
    let lastX = 0;
    let lastY = 0;
    let theta = Math.atan2(camera.position.x, camera.position.z);
    let phi = Math.atan2(
      Math.hypot(camera.position.x, camera.position.z),
      camera.position.y - 2.4,
    );
    let radius = camera.position.length();
    const target = new THREE.Vector3(0, 2.4, 0);

    function applyCam() {
      camera.position.x = target.x + radius * Math.sin(phi) * Math.sin(theta);
      camera.position.z = target.z + radius * Math.sin(phi) * Math.cos(theta);
      camera.position.y = target.y + radius * Math.cos(phi);
      camera.lookAt(target);
    }
    applyCam();

    function onDown(e: PointerEvent) {
      dragging = true;
      lastX = e.clientX;
      lastY = e.clientY;
    }
    function onUp() {
      dragging = false;
    }
    function onMove(e: PointerEvent) {
      if (!dragging) return;
      const dx = e.clientX - lastX;
      const dy = e.clientY - lastY;
      lastX = e.clientX;
      lastY = e.clientY;
      theta -= dx * 0.005;
      phi = Math.max(0.15, Math.min(Math.PI - 0.15, phi - dy * 0.005));
      applyCam();
    }
    function onWheel(e: WheelEvent) {
      e.preventDefault();
      radius = Math.max(6, Math.min(28, radius * (1 + e.deltaY * 0.001)));
      applyCam();
    }

    renderer.domElement.addEventListener("pointerdown", onDown);
    window.addEventListener("pointerup", onUp);
    window.addEventListener("pointermove", onMove);
    renderer.domElement.addEventListener("wheel", onWheel, { passive: false });

    // ── Render loop with idle auto-rotate ────────────────────────────
    let raf = 0;
    let idleSince = performance.now();
    function tick() {
      const now = performance.now();
      if (now - idleSince > 1800 && !dragging) {
        theta -= 0.0008;
        applyCam();
      }
      renderer.render(scene, camera);
      raf = requestAnimationFrame(tick);
    }
    tick();

    // ── Resize ───────────────────────────────────────────────────────
    const ro = new ResizeObserver(() => {
      const w = mountRef.clientWidth;
      const h = mountRef.clientHeight;
      camera.aspect = w / h;
      camera.updateProjectionMatrix();
      renderer.setSize(w, h);
    });
    ro.observe(mountRef);

    onCleanup(() => {
      cancelAnimationFrame(raf);
      ro.disconnect();
      renderer.domElement.removeEventListener("pointermove", onPointerMove);
      renderer.domElement.removeEventListener("click", onClick);
      renderer.domElement.removeEventListener("pointerdown", onDown);
      window.removeEventListener("pointerup", onUp);
      window.removeEventListener("pointermove", onMove);
      renderer.domElement.removeEventListener("wheel", onWheel);
      if (renderer.domElement.parentNode === mountRef) {
        mountRef.removeChild(renderer.domElement);
      }
      for (const m of nodeMeshes.values()) {
        m.geometry.dispose();
        (m.material as THREE.Material).dispose();
      }
      for (const h of haloMeshes.values()) {
        h.geometry.dispose();
        (h.material as THREE.Material).dispose();
      }
      for (const l of edgeLines.values()) {
        l.geometry.dispose();
        (l.material as THREE.Material).dispose();
      }
      renderer.dispose();
    });
  });

  // ── Rebuild node/edge meshes when props change ───────────────────────
  createEffect(() => {
    // Drop old meshes
    for (const m of nodeMeshes.values()) {
      nodeGroup.remove(m);
      m.geometry.dispose();
      (m.material as THREE.Material).dispose();
    }
    nodeMeshes.clear();
    for (const h of haloMeshes.values()) {
      haloGroup.remove(h);
      h.geometry.dispose();
      (h.material as THREE.Material).dispose();
    }
    haloMeshes.clear();

    const ns = nodes();
    for (const n of ns) {
      const mat = new THREE.MeshBasicMaterial({
        color: new THREE.Color(TINT[n.kind]),
        transparent: true,
        opacity: 0.92,
      });
      const mesh = new THREE.Mesh(geometryFor(n.kind), mat);
      mesh.position.set(n.x, LAYER_Z[n.kind], n.y);
      if (n.kind === "Perspective") mesh.rotation.x = Math.PI;
      mesh.userData.id = n.id;
      nodeGroup.add(mesh);
      nodeMeshes.set(n.id, mesh);

      if (n.kind !== "Fact") {
        const halo = new THREE.Mesh(
          new THREE.RingGeometry(0.22, 0.27, 24),
          new THREE.MeshBasicMaterial({
            color: new THREE.Color(TINT[n.kind]),
            transparent: true,
            opacity: 0.18,
            side: THREE.DoubleSide,
          }),
        );
        halo.position.set(n.x, LAYER_Z[n.kind] + 0.001, n.y);
        halo.rotation.x = -Math.PI / 2;
        haloGroup.add(halo);
        haloMeshes.set(n.id, halo);
      }
    }
  });

  createEffect(() => {
    for (const l of edgeLines.values()) {
      edgeGroup.remove(l);
      l.geometry.dispose();
      (l.material as THREE.Material).dispose();
    }
    edgeLines.clear();

    const byId = new Map(nodes().map((n) => [n.id, n]));
    for (const e of edges()) {
      const a = byId.get(e.src);
      const b = byId.get(e.tgt);
      if (!a || !b) continue;
      const geom = new THREE.BufferGeometry().setFromPoints([
        new THREE.Vector3(a.x, LAYER_Z[a.kind], a.y),
        new THREE.Vector3(b.x, LAYER_Z[b.kind], b.y),
      ]);
      const mat = new THREE.LineBasicMaterial({
        color: 0x6b7280,
        transparent: true,
        opacity: 0.22,
      });
      const line = new THREE.Line(geom, mat);
      line.userData.id = e.id;
      edgeGroup.add(line);
      edgeLines.set(e.id, line);
    }
  });

  // ── Focus highlighting + filter visibility (chain lit, rest ghosted) ─
  createEffect(() => {
    const focus = focusId();
    const c = chain();
    const hasFocus = c.nodes.size > 1;
    const ids = byId();

    for (const n of nodes()) {
      const m = nodeMeshes.get(n.id);
      const halo = haloMeshes.get(n.id);
      if (!m) continue;
      const mat = m.material as THREE.MeshBasicMaterial;
      const visible = passKind(n.kind) && passFlavor(n.flavor);
      let opacity: number;
      if (!visible) opacity = 0.04;
      else if (hasFocus) opacity = c.nodes.has(n.id) ? 0.98 : 0.1;
      else opacity = 0.92;
      mat.opacity = opacity;
      if (halo) {
        const haloMat = halo.material as THREE.MeshBasicMaterial;
        haloMat.opacity = opacity * 0.22;
      }
      m.scale.setScalar(focus === n.id ? 1.6 : 1.0);
    }

    for (const e of edges()) {
      const line = edgeLines.get(e.id);
      if (!line) continue;
      const a = ids.get(e.src);
      const b = ids.get(e.tgt);
      if (!a || !b) continue;
      const aV = passKind(a.kind) && passFlavor(a.flavor);
      const bV = passKind(b.kind) && passFlavor(b.flavor);
      const mat = line.material as THREE.LineBasicMaterial;
      let opacity: number;
      if (!aV || !bV) opacity = 0.02;
      else if (hasFocus) opacity = c.edges.has(e.id) ? 0.85 : 0.04;
      else opacity = 0.22;
      mat.opacity = opacity;
    }
  });

  return (
    <div class="atlas-shell">
      <div class="atlas-chrome">
        <div class="atlas-chrome-l">
          <span class="atlas-mark">⌬</span>
          <span class="atlas-name">Proxima · Atlas</span>
          <span class="atlas-sub">embedding-projected memory map</span>
        </div>
        <div class="atlas-chrome-r">
          <span class="atlas-stat">
            <span class="k">nodes</span>{" "}
            <span class="v">{nodes().length}</span>
          </span>
          <span class="atlas-stat">
            <span class="k">edges</span>{" "}
            <span class="v">{edges().length}</span>
          </span>
          <span class="atlas-stat">
            <span class="k">flavors</span>{" "}
            <span class="v">{props.hub.registeredFlavors().length}</span>
          </span>
        </div>
      </div>

      <div class="atlas-body">
        <div class="atlas-filters">
          <div class="filter-section">
            <div class="filter-head">layer</div>
            <Pill
              active={showFact()}
              onClick={() => setShowFact((v) => !v)}
              color={TINT_HEX.Fact}
              count={counts().kind.Fact}
            >
              F · Facts
            </Pill>
            <Pill
              active={showAbs()}
              onClick={() => setShowAbs((v) => !v)}
              color={TINT_HEX.Abstraction}
              count={counts().kind.Abstraction}
            >
              A · Abstractions
            </Pill>
            <Pill
              active={showPersp()}
              onClick={() => setShowPersp((v) => !v)}
              color={TINT_HEX.Perspective}
              count={counts().kind.Perspective}
            >
              P · Perspectives
            </Pill>
            <Pill
              active={showGoal()}
              onClick={() => setShowGoal((v) => !v)}
              color={TINT_HEX.Goal}
              count={counts().kind.Goal}
            >
              G · Goals
            </Pill>
          </div>

          <div class="filter-section">
            <div class="filter-head">flavor</div>
            <Show
              when={props.hub.registeredFlavors().length > 0}
              fallback={
                <div class="filter-note">
                  No flavors registered. Bare substrate.
                </div>
              }
            >
              <For each={props.hub.registeredFlavors()}>
                {(f) => (
                  <Pill
                    active={!hiddenFlavors().has(f)}
                    onClick={() => toggleFlavor(f)}
                    color={TINT_HEX.Abstraction}
                    count={counts().flavor[f] ?? 0}
                  >
                    ƒ:{f}
                  </Pill>
                )}
              </For>
            </Show>
          </div>
        </div>

        <div class="atlas-canvas-wrap">
          <div class="atlas-canvas" ref={mountRef} />
          <div class="atlas-overlay-tl">
            <div class="ov-row">
              z = layer (locked) — F=0, A=1.6, P=3.2, G=4.8
            </div>
            <div class="ov-row faint">
              x,y = shared embedding projection · sticky · re-seed nightly
            </div>
          </div>
        </div>

        <Inspector
          hub={props.hub}
          node={focusNode()}
          adj={adj()}
          byId={byId()}
          onPickNode={setPickedId}
        />
      </div>
    </div>
  );
};
