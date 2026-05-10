import ELK, { type ElkNode, type LayoutOptions } from "elkjs/lib/elk.bundled.js";
import type { PersonalityInstanceTs } from "../../bindings";
import {
  paletteKey,
  type BuildModelInput,
  type CanvasEdge,
  type CanvasModel,
  type CanvasNode,
  type ProducesByPaletteKey,
} from "./types";

const elk = new ELK();

const PERSONALITY_BASE_WIDTH = 280;
const PERSONALITY_HEADER_HEIGHT = 78;
const PERSONALITY_ENTRY_HEIGHT = 30;
const PERSONALITY_EMPTY_BODY_HEIGHT = 32;
const PERSONALITY_PADDING = 16;
const SCHEMA_NODE_WIDTH = 200;
const SCHEMA_NODE_HEIGHT = 36;
const RELATION_NODE_WIDTH = 200;
const RELATION_NODE_HEIGHT = 28;

const layoutOptions: LayoutOptions = {
  "elk.algorithm": "layered",
  "elk.direction": "RIGHT",
  "elk.layered.spacing.nodeNodeBetweenLayers": "80",
  "elk.spacing.nodeNode": "32",
  "elk.layered.nodePlacement.strategy": "NETWORK_SIMPLEX",
  "elk.edgeRouting": "ORTHOGONAL",
  "elk.padding": "[top=24,left=24,bottom=24,right=24]",
};

const personalityHeight = (instance: PersonalityInstanceTs): number => {
  const entries = instance.wake_entries.length;
  const body =
    entries === 0
      ? PERSONALITY_EMPTY_BODY_HEIGHT
      : entries * PERSONALITY_ENTRY_HEIGHT;
  return PERSONALITY_HEADER_HEIGHT + body + PERSONALITY_PADDING;
};

const schemaNodeId = (schemaId: string): string => `schema:${schemaId}`;
const relationNodeId = (relationId: string): string => `relation:${relationId}`;
const personalityNodeId = (id: string): string => `personality:${id}`;
const wakeInPortId = (instanceId: string, index: number): string =>
  `port-in:${instanceId}:${index}`;
const wakeOutPortId = (instanceId: string, index: number): string =>
  `port-out:${instanceId}:${index}`;

const lookupProduces = (
  map: ProducesByPaletteKey,
  palette: string[],
): { schema_ids: string[]; relation_ids: string[] } => {
  const found = map.get(paletteKey(palette));
  return found ?? { schema_ids: [], relation_ids: [] };
};

interface PendingTrigger {
  source: string;
  target: string;
  shapeId: string;
  instanceId: string;
  entryIndex: number;
}

interface PendingProduces {
  source: string;
  target: string;
  shapeId: string;
  instanceId: string;
  entryIndex: number;
}

export const computeLayout = async (
  input: BuildModelInput,
): Promise<CanvasModel> => {
  // ── Collect shape sets (deduplicated) ─────────────────────────────
  const triggerSchemas = new Set<string>();
  const producedSchemas = new Set<string>();
  const producedRelations = new Set<string>();

  for (const instance of input.instances) {
    for (const entry of instance.wake_entries) {
      if (entry.trigger_kind === "on_memory" && entry.trigger_id.trim() !== "") {
        triggerSchemas.add(entry.trigger_id);
      }
      // on_edge triggers consume relations; render them as relation nodes
      // for symmetry with produced relations (loop closure works the same).
      if (entry.trigger_kind === "on_edge" && entry.trigger_id.trim() !== "") {
        producedRelations.add(entry.trigger_id);
      }
      const produces = lookupProduces(
        input.producesByPaletteKey,
        entry.substrate_tool_palette,
      );
      for (const schemaId of produces.schema_ids) producedSchemas.add(schemaId);
      for (const relationId of produces.relation_ids) producedRelations.add(relationId);
    }
  }

  // Schema nodes are the union of triggers + produced (loop closure dedup).
  const allSchemas = new Set<string>([...triggerSchemas, ...producedSchemas]);

  // ── Build ELK graph ───────────────────────────────────────────────
  const elkNodes: ElkNode[] = [];
  for (const schemaId of allSchemas) {
    elkNodes.push({
      id: schemaNodeId(schemaId),
      width: SCHEMA_NODE_WIDTH,
      height: SCHEMA_NODE_HEIGHT,
    });
  }
  for (const relationId of producedRelations) {
    elkNodes.push({
      id: relationNodeId(relationId),
      width: RELATION_NODE_WIDTH,
      height: RELATION_NODE_HEIGHT,
    });
  }
  for (const instance of input.instances) {
    elkNodes.push({
      id: personalityNodeId(instance.personality_instance_id),
      width: PERSONALITY_BASE_WIDTH,
      height: personalityHeight(instance),
      ports: instance.wake_entries.flatMap((_, index) => [
        {
          id: wakeInPortId(instance.personality_instance_id, index),
          layoutOptions: { "port.side": "WEST" },
        },
        {
          id: wakeOutPortId(instance.personality_instance_id, index),
          layoutOptions: { "port.side": "EAST" },
        },
      ]),
      layoutOptions: {
        portConstraints: "FIXED_ORDER",
      },
    });
  }

  const elkEdges: NonNullable<ElkNode["edges"]> = [];
  let edgeCounter = 0;
  const pendingTriggers: PendingTrigger[] = [];
  const pendingProduces: PendingProduces[] = [];

  for (const instance of input.instances) {
    instance.wake_entries.forEach((entry, index) => {
      // Trigger edges — Schema/Relation → IN port
      const trigger = entry.trigger_id.trim();
      if (trigger !== "") {
        const isMemory = entry.trigger_kind === "on_memory";
        const sourceNodeId = isMemory
          ? schemaNodeId(trigger)
          : relationNodeId(trigger);
        const target = wakeInPortId(instance.personality_instance_id, index);
        const id = `edge:t:${edgeCounter++}`;
        elkEdges.push({ id, sources: [sourceNodeId], targets: [target] });
        pendingTriggers.push({
          source: sourceNodeId,
          target,
          shapeId: trigger,
          instanceId: instance.personality_instance_id,
          entryIndex: index,
        });
      }

      // Produces edges — OUT port → Schema/Relation
      const produces = lookupProduces(
        input.producesByPaletteKey,
        entry.substrate_tool_palette,
      );
      const source = wakeOutPortId(instance.personality_instance_id, index);
      for (const schemaId of produces.schema_ids) {
        const target = schemaNodeId(schemaId);
        const id = `edge:p:${edgeCounter++}`;
        elkEdges.push({ id, sources: [source], targets: [target] });
        pendingProduces.push({
          source,
          target,
          shapeId: schemaId,
          instanceId: instance.personality_instance_id,
          entryIndex: index,
        });
      }
      for (const relationId of produces.relation_ids) {
        const target = relationNodeId(relationId);
        const id = `edge:p:${edgeCounter++}`;
        elkEdges.push({ id, sources: [source], targets: [target] });
        pendingProduces.push({
          source,
          target,
          shapeId: relationId,
          instanceId: instance.personality_instance_id,
          entryIndex: index,
        });
      }
    });
  }

  const elkGraph: ElkNode = {
    id: "personality-graph",
    layoutOptions,
    children: elkNodes,
    edges: elkEdges,
  };

  const positioned = await elk.layout(elkGraph);

  // ── Project positioned ELK back into CanvasModel ─────────────────
  const nodeById = new Map<string, ElkNode>();
  for (const child of positioned.children ?? []) {
    nodeById.set(child.id, child);
  }

  const nodes: CanvasNode[] = [];
  for (const schemaId of allSchemas) {
    const node = nodeById.get(schemaNodeId(schemaId));
    if (!node) continue;
    nodes.push({
      id: schemaNodeId(schemaId),
      kind: "schema",
      x: node.x ?? 0,
      y: node.y ?? 0,
      width: node.width ?? SCHEMA_NODE_WIDTH,
      height: node.height ?? SCHEMA_NODE_HEIGHT,
      data: { kind: "schema", schema_id: schemaId },
    });
  }
  for (const relationId of producedRelations) {
    const node = nodeById.get(relationNodeId(relationId));
    if (!node) continue;
    nodes.push({
      id: relationNodeId(relationId),
      kind: "relation",
      x: node.x ?? 0,
      y: node.y ?? 0,
      width: node.width ?? RELATION_NODE_WIDTH,
      height: node.height ?? RELATION_NODE_HEIGHT,
      data: { kind: "relation", relation_id: relationId },
    });
  }
  for (const instance of input.instances) {
    const node = nodeById.get(personalityNodeId(instance.personality_instance_id));
    if (!node) continue;
    nodes.push({
      id: personalityNodeId(instance.personality_instance_id),
      kind: "personality",
      x: node.x ?? 0,
      y: node.y ?? 0,
      width: node.width ?? PERSONALITY_BASE_WIDTH,
      height: node.height ?? personalityHeight(instance),
      data: { kind: "personality", instance },
    });
  }

  const triggerCount = pendingTriggers.length;
  const edges: CanvasEdge[] = [];
  positioned.edges?.forEach((rawEdge, index) => {
    const sections = rawEdge.sections ?? [];
    const path = sections
      .map((section) => {
        const parts: string[] = [];
        parts.push(`M ${section.startPoint.x} ${section.startPoint.y}`);
        for (const bend of section.bendPoints ?? []) {
          parts.push(`L ${bend.x} ${bend.y}`);
        }
        parts.push(`L ${section.endPoint.x} ${section.endPoint.y}`);
        return parts.join(" ");
      })
      .join(" ");
    if (index < triggerCount) {
      const meta = pendingTriggers[index];
      edges.push({
        kind: "trigger",
        id: rawEdge.id,
        source: meta.source,
        target: meta.target,
        shape_id: meta.shapeId,
        src_instance_id: "",
        tgt_instance_id: meta.instanceId,
        tgt_entry_index: meta.entryIndex,
        path,
      });
    } else {
      const meta = pendingProduces[index - triggerCount];
      edges.push({
        kind: "produces",
        id: rawEdge.id,
        source: meta.source,
        target: meta.target,
        shape_id: meta.shapeId,
        src_instance_id: meta.instanceId,
        src_entry_index: meta.entryIndex,
        tgt_instance_id: "",
        tgt_entry_index: -1,
        path,
      });
    }
  });

  return {
    nodes,
    edges,
    width: positioned.width ?? 0,
    height: positioned.height ?? 0,
  };
};
