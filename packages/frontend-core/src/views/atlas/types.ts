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

// ── Adjacency + chain traversal ────────────────────────────────────────
export interface OutEntry {
  tgt: string;
  id: string;
  kind: string;
}
export interface InEntry {
  src: string;
  id: string;
  kind: string;
}
export interface Adjacency {
  out: Map<string, OutEntry[]>;
  inn: Map<string, InEntry[]>;
}

export interface Chain {
  nodes: Set<string>;
  edges: Set<string>;
}
