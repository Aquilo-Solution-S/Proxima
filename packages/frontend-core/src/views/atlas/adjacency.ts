import type { Adjacency, AtlasEdge, Chain, InEntry, OutEntry } from "./types";

export function buildAdjacency(edges: AtlasEdge[]): Adjacency {
  const out = new Map<string, OutEntry[]>();
  const inn = new Map<string, InEntry[]>();
  for (const e of edges) {
    if (!out.has(e.src)) out.set(e.src, []);
    if (!inn.has(e.tgt)) inn.set(e.tgt, []);
    out
      .get(e.src)!
      .push({
        tgt: e.tgt,
        id: e.id,
        kind: e.kind,
        relationClass: e.relationClass,
      });
    inn
      .get(e.tgt)!
      .push({
        src: e.src,
        id: e.id,
        kind: e.kind,
        relationClass: e.relationClass,
      });
  }
  return { out, inn };
}

export function chainOf(nodeId: string, adj: Adjacency, depth = 5): Chain {
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
