import "../atlas.css";

import { For, Show, type Component, createEffect, createMemo, createSignal, onCleanup, onMount } from "solid-js";
import * as THREE from "three";
import { GRAPH_SNAPSHOT_LIMIT, MAX_SNAPSHOT_EDGES, useGraph } from "../../graph-store";
import { flavorFilterId, useGraphFilter } from "../../graph-filter-store";
import { filterGraphSnapshot } from "../../graph-selectors";
import type { Hub } from "../../hub";
import { buildAdjacency, chainOf } from "./adjacency";
import { Inspector, Pill } from "./inspector";
import { atlasProjectionFromGraph } from "./projection";
import type { AtlasEdge, AtlasNode, AtlasNodeKind, Chain } from "./types";
import { geometryFor, LAYER_LABELS, LAYER_Z, makeLayerLabel, TINT, TINT_HEX } from "./three-helpers";

export type { AtlasEdge, AtlasNode, AtlasNodeKind } from "./types";
export type { AtlasProjection } from "./projection";

const BRIGHT_TINT = new THREE.Color(0xffffff);

export const Atlas: Component<{
  hub: Hub;
  nodes?: AtlasNode[];
  edges?: AtlasEdge[];
}> = (props) => {
  const propOverride = () => props.nodes !== undefined || props.edges !== undefined;
  const graph = propOverride() ? null : useGraph();
  const filters = useGraphFilter();
  const filtered = createMemo(() =>
    graph === null
      ? null
      : filterGraphSnapshot(graph.state(), filters.state(), props.hub),
  );
  const projected = createMemo(() =>
    propOverride()
      ? { nodes: props.nodes ?? [], edges: props.edges ?? [], omittedEdgeCount: 0 }
      : atlasProjectionFromGraph(filtered()!, props.hub),
  );
  const nodes = () => projected().nodes;
  const edges = () => projected().edges;

  const [hoverId, setHoverId] = createSignal<string | null>(null);
  const [pickedId, setPickedId] = createSignal<string | null>(null);
  const [pickHistory, setPickHistory] = createSignal<string[]>([]);
  const [pickHistoryIndex, setPickHistoryIndex] = createSignal(-1);

  const passKind = (k: AtlasNodeKind) => filters.state().layers.has(k);
  const passFlavor = (f: string | null) =>
    !filters.state().hiddenFlavorIds.has(flavorFilterId(f));

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
  const canGoBack = () => pickHistoryIndex() > 0;
  const canGoForward = () => {
    const index = pickHistoryIndex();
    return index >= 0 && index < pickHistory().length - 1;
  };

  function pickNode(id: string) {
    const current = pickedId();
    if (current === id) return;
    const index = pickHistoryIndex();
    const next = [...pickHistory().slice(0, index + 1), id];
    setPickHistory(next);
    setPickHistoryIndex(next.length - 1);
    setPickedId(id);
  }

  function goPickHistory(delta: -1 | 1) {
    const nextIndex = pickHistoryIndex() + delta;
    const nextId = pickHistory()[nextIndex];
    if (nextId === undefined) return;
    setPickHistoryIndex(nextIndex);
    setPickedId(nextId);
    setHoverId(null);
  }

  function keyIsEditableTarget(target: EventTarget | null) {
    if (!(target instanceof HTMLElement)) return false;
    const tag = target.tagName;
    return target.isContentEditable || tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT";
  }

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

  const rawGraphCount = () => {
    if (graph === null) return (props.nodes?.length ?? 0) + (props.edges?.length ?? 0);
    const snapshot = graph.state();
    return snapshot.memoriesById.size + snapshot.goalsById.size + snapshot.edgesById.size;
  };

  const nodeWindowIsTruncated = () => {
    if (graph === null) return false;
    const snapshot = graph.state();
    return (
      snapshot.memoriesById.size >= GRAPH_SNAPSHOT_LIMIT ||
      snapshot.goalsById.size >= GRAPH_SNAPSHOT_LIMIT
    );
  };

  const edgeCapIsHit = () => {
    if (graph === null) return false;
    return graph.state().edgesById.size >= MAX_SNAPSHOT_EDGES;
  };

  const canvasMessage = () => {
    if (graph !== null && graph.state().streamStatus === "connecting") {
      return "Loading graph";
    }
    if (graph !== null && graph.state().streamStatus === "degraded") {
      return "Graph stream degraded";
    }
    if (rawGraphCount() === 0) return "No graph rows";
    if (nodes().length === 0) return "No rows match filters";
    return null;
  };

  const statusPills = createMemo(() => {
    if (graph === null) return [];
    const snapshot = graph.state();
    const pills: string[] = [];
    if (projected().omittedEdgeCount > 0) {
      pills.push(`${projected().omittedEdgeCount} edge endpoints unavailable`);
    }
    if (snapshot.decodeErrorsByEntity.size > 0) {
      pills.push(`${snapshot.decodeErrorsByEntity.size} payload decode errors`);
    }
    if (nodeWindowIsTruncated()) {
      pills.push(`snapshot truncated at ${GRAPH_SNAPSHOT_LIMIT} nodes`);
    }
    if (edgeCapIsHit()) {
      pills.push(`edges truncated at ${MAX_SNAPSHOT_EDGES}`);
    }
    return pills;
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
      if (id) pickNode(id);
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

  createEffect(() => {
    const liveIds = byId();
    const id = pickedId();
    if (id !== null && !liveIds.has(id)) {
      setPickedId(null);
      setHoverId(null);
      setPickHistory((history) => {
        const next = history.filter((entry) => liveIds.has(entry));
        setPickHistoryIndex(Math.min(pickHistoryIndex(), next.length - 1));
        return next;
      });
    }
  });

  onMount(() => {
    function onKeyDown(event: KeyboardEvent) {
      if (keyIsEditableTarget(event.target)) return;
      const wantsBack =
        event.key === "BrowserBack" ||
        (event.key === "ArrowLeft" && (event.altKey || event.metaKey)) ||
        (event.key === "[" && event.metaKey);
      const wantsForward =
        event.key === "BrowserForward" ||
        (event.key === "ArrowRight" && (event.altKey || event.metaKey)) ||
        (event.key === "]" && event.metaKey);
      if (wantsBack && canGoBack()) {
        event.preventDefault();
        goPickHistory(-1);
      } else if (wantsForward && canGoForward()) {
        event.preventDefault();
        goPickHistory(1);
      }
    }
    window.addEventListener("keydown", onKeyDown);
    onCleanup(() => window.removeEventListener("keydown", onKeyDown));
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
    const hasFocus = focus !== null;
    const ids = byId();

    for (const n of nodes()) {
      const m = nodeMeshes.get(n.id);
      const halo = haloMeshes.get(n.id);
      if (!m) continue;
      const mat = m.material as THREE.MeshBasicMaterial;
      const visible = passKind(n.kind) && passFlavor(n.flavor);
      const inChain = c.nodes.has(n.id);
      const isFocus = focus === n.id;
      let opacity: number;
      if (!visible) opacity = 0.04;
      else if (hasFocus) opacity = inChain ? 1 : 0.1;
      else opacity = 0.96;
      mat.opacity = opacity;
      mat.color.set(TINT[n.kind]);
      if (visible && inChain) {
        mat.color.lerp(BRIGHT_TINT, isFocus ? 0.58 : 0.3);
      }
      if (halo) {
        const haloMat = halo.material as THREE.MeshBasicMaterial;
        haloMat.opacity = visible && isFocus ? 0.72 : opacity * 0.28;
        haloMat.color.set(TINT[n.kind]);
        if (visible && inChain) {
          haloMat.color.lerp(BRIGHT_TINT, isFocus ? 0.62 : 0.34);
        }
      }
      m.scale.setScalar(isFocus ? 1.85 : inChain && hasFocus ? 1.18 : 1.0);
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
      const inChain = c.edges.has(e.id);
      let opacity: number;
      if (!aV || !bV) opacity = 0.02;
      else if (hasFocus) opacity = inChain ? 0.96 : 0.04;
      else opacity = 0.3;
      mat.opacity = opacity;
      mat.color.set(inChain && aV && bV ? 0xe8e4d6 : 0x8794b0);
    }
  });

  return (
    <div class="atlas-shell">
      <div class="atlas-chrome">
        <div class="atlas-chrome-l">
          <span class="atlas-mark">⌬</span>
          <span class="atlas-name">Proxima · Atlas</span>
          <span class="atlas-sub">deterministic memory map</span>
        </div>
        <div class="atlas-chrome-r">
          <div class="atlas-nav" aria-label="Atlas node history">
            <button
              type="button"
              class="atlas-nav-btn"
              title="Back (Alt+Left, Cmd+Left, or Cmd+[)"
              aria-label="Back"
              disabled={!canGoBack()}
              onClick={() => goPickHistory(-1)}
            >
              ‹
            </button>
            <button
              type="button"
              class="atlas-nav-btn"
              title="Forward (Alt+Right, Cmd+Right, or Cmd+])"
              aria-label="Forward"
              disabled={!canGoForward()}
              onClick={() => goPickHistory(1)}
            >
              ›
            </button>
          </div>
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
              active={filters.state().layers.has("Fact")}
              onClick={() => filters.setLayer("Fact", !filters.state().layers.has("Fact"))}
              color={TINT_HEX.Fact}
              count={counts().kind.Fact}
            >
              F · Facts
            </Pill>
            <Pill
              active={filters.state().layers.has("Abstraction")}
              onClick={() =>
                filters.setLayer("Abstraction", !filters.state().layers.has("Abstraction"))
              }
              color={TINT_HEX.Abstraction}
              count={counts().kind.Abstraction}
            >
              A · Abstractions
            </Pill>
            <Pill
              active={filters.state().layers.has("Perspective")}
              onClick={() =>
                filters.setLayer("Perspective", !filters.state().layers.has("Perspective"))
              }
              color={TINT_HEX.Perspective}
              count={counts().kind.Perspective}
            >
              P · Perspectives
            </Pill>
            <Pill
              active={filters.state().layers.has("Goal")}
              onClick={() => filters.setLayer("Goal", !filters.state().layers.has("Goal"))}
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
                    active={!filters.state().hiddenFlavorIds.has(f)}
                    onClick={() =>
                      filters.setFlavor(f, filters.state().hiddenFlavorIds.has(f))
                    }
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
          <Show when={canvasMessage()}>
            {(message) => <div class="atlas-loading">{message()}</div>}
          </Show>
          <div class="atlas-overlay-tl">
            <div class="ov-row">
              z = layer (locked) — F=0, A=1.6, P=3.2, G=4.8
            </div>
            <div class="ov-row faint">
              x,y = deterministic projection
            </div>
          </div>
          <Show when={statusPills().length > 0}>
            <div class="atlas-status-pills">
              <For each={statusPills()}>
                {(pill) => <span class="atlas-status-pill">{pill}</span>}
              </For>
            </div>
          </Show>
        </div>

        <Inspector
          hub={props.hub}
          node={focusNode()}
          adj={adj()}
          byId={byId()}
          onPickNode={pickNode}
        />
      </div>
    </div>
  );
};
