import * as THREE from "three";
import type { AtlasNodeKind } from "./types";

// ── Layer + tint discipline ────────────────────────────────────────────
export const LAYER_Z: Record<AtlasNodeKind, number> = {
  Fact: 0,
  Abstraction: 1.6,
  Perspective: 3.2,
  Goal: 4.8,
};

export const TINT: Record<AtlasNodeKind, number> = {
  Fact: 0xa8aeba,
  Abstraction: 0xc9a86a,
  Perspective: 0xe8e4d6,
  Goal: 0xd9c28a,
};

export const TINT_HEX: Record<AtlasNodeKind, string> = {
  Fact: "#A8AEBA",
  Abstraction: "#C9A86A",
  Perspective: "#E8E4D6",
  Goal: "#D9C28A",
};

export const KIND_GLYPH: Record<AtlasNodeKind, string> = {
  Fact: "◆",
  Abstraction: "△",
  Perspective: "▽",
  Goal: "◇",
};

export const LAYER_LABELS: Array<{ z: number; t: string; c: string }> = [
  { z: 0, t: "F · Facts", c: TINT_HEX.Fact },
  { z: 1.6, t: "A · Abstractions", c: TINT_HEX.Abstraction },
  { z: 3.2, t: "P · Perspectives", c: TINT_HEX.Perspective },
  { z: 4.8, t: "G · Goals", c: TINT_HEX.Goal },
];

// ── Geometry per kind ──────────────────────────────────────────────────
export function geometryFor(kind: AtlasNodeKind): THREE.BufferGeometry {
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
export function makeLayerLabel(text: string, color: string): THREE.Sprite {
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
