/*
 * PLACEHOLDER DATA STATE
 * This component renders the substrate Atlas — three.js scene with locked
 * z-by-layer (F=0, A=1.6, P=3.2, G=4.8), camera, orbit, picking, and
 * uniform-grey edges. nodes/edges default to []; filter rail and Inspector
 * panel are 2e.3, real data wiring (commands.atlas) is a follow-up that
 * needs backend embedding positions first.
 */

import {
  type Component,
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

const LAYER_LABELS: Array<{ z: number; t: string; c: string }> = [
  { z: 0, t: "F · Facts", c: "#A8AEBA" },
  { z: 1.6, t: "A · Abstractions", c: "#C9A86A" },
  { z: 3.2, t: "P · Perspectives", c: "#E8E4D6" },
  { z: 4.8, t: "G · Goals", c: "#D9C28A" },
];

// ── Adjacency + chain traversal ────────────────────────────────────────
interface Adjacency {
  out: Map<string, Array<{ tgt: string; id: string }>>;
  inn: Map<string, Array<{ src: string; id: string }>>;
}

function buildAdjacency(edges: AtlasEdge[]): Adjacency {
  const out = new Map<string, Array<{ tgt: string; id: string }>>();
  const inn = new Map<string, Array<{ src: string; id: string }>>();
  for (const e of edges) {
    if (!out.has(e.src)) out.set(e.src, []);
    if (!inn.has(e.tgt)) inn.set(e.tgt, []);
    out.get(e.src)!.push({ tgt: e.tgt, id: e.id });
    inn.get(e.tgt)!.push({ src: e.src, id: e.id });
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

// ── Sprite labels for layers ───────────────────────────────────────────
function makeLayerLabel(text: string, color: string): THREE.Sprite {
  const c = document.createElement("canvas");
  c.width = 256;
  c.height = 64;
  const ctx = c.getContext("2d")!;
  ctx.font = "600 28px 'JetBrains Mono', monospace";
  ctx.fillStyle = color;
  ctx.textBaseline = "middle";
  ctx.fillText(text, 8, 32);
  const tex = new THREE.CanvasTexture(c);
  tex.minFilter = THREE.LinearFilter;
  const mat = new THREE.SpriteMaterial({ map: tex, transparent: true, opacity: 0.7 });
  const sp = new THREE.Sprite(mat);
  sp.scale.set(2.8, 0.7, 1);
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
    scene.background = new THREE.Color(0x0e1013);
    scene.fog = new THREE.Fog(0x0e1013, 14, 32);

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
      const grid = new THREE.GridHelper(20, 20, 0x1f232b, 0x14171c);
      grid.position.y = z;
      const mat = grid.material as THREE.Material;
      mat.transparent = true;
      mat.opacity = 0.35;
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

  // ── Focus highlighting (chain lit, rest ghosted) ─────────────────────
  createEffect(() => {
    const focus = focusId();
    const c = chain();
    const hasFocus = c.nodes.size > 1;

    for (const n of nodes()) {
      const m = nodeMeshes.get(n.id);
      const halo = haloMeshes.get(n.id);
      if (!m) continue;
      const mat = m.material as THREE.MeshBasicMaterial;
      const opacity = hasFocus ? (c.nodes.has(n.id) ? 0.98 : 0.1) : 0.92;
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
      const mat = line.material as THREE.LineBasicMaterial;
      mat.opacity = hasFocus ? (c.edges.has(e.id) ? 0.85 : 0.04) : 0.22;
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
        {/* Filter rail — 2e.3 wires kind / flavor pills here */}
        <div class="atlas-filters" />

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

        {/* Inspector — 2e.3 dispatches via hub.rendererFor for selected node */}
        <div class="atlas-inspector empty" />
      </div>
    </div>
  );
};
