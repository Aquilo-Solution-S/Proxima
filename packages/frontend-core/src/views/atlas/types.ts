import type { GoalRow, MemoryRow } from "../../bindings";
import type { DecodeError } from "../../graph-store";

// ── Substrate types ─────────────────────────────────────────────────────
export type AtlasNodeKind = "Fact" | "Abstraction" | "Perspective" | "Goal";

export interface AtlasNode {
  id: string;
  kind: AtlasNodeKind;
  schemaId: string;
  schemaVersion: number;
  flavor: string | null;
  x: number; // deterministic projection x
  y: number; // deterministic projection y
  title?: string;
  memory?: MemoryRow;
  goal?: GoalRow;
  payload?: unknown;
  decodeError?: DecodeError;
}

export interface AtlasEdge {
  id: string;
  src: string;
  tgt: string;
  kind: string;
  relationClass?: string;
}

// ── Adjacency + chain traversal ────────────────────────────────────────
export interface OutEntry {
  tgt: string;
  id: string;
  kind: string;
  relationClass?: string;
}
export interface InEntry {
  src: string;
  id: string;
  kind: string;
  relationClass?: string;
}
export interface Adjacency {
  out: Map<string, OutEntry[]>;
  inn: Map<string, InEntry[]>;
}

export interface Chain {
  nodes: Set<string>;
  edges: Set<string>;
}
