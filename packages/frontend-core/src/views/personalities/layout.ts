import ELK, { type ElkNode, type LayoutOptions } from "elkjs/lib/elk.bundled.js";
import type { PersonalityInstanceTs } from "../../bindings";
import type { CanvasEdge, CanvasModel, CanvasNode } from "./types";

const elk = new ELK();

const PERSONALITY_BASE_WIDTH = 280;
const PERSONALITY_HEADER_HEIGHT = 78;
const PERSONALITY_ENTRY_HEIGHT = 30;
const PERSONALITY_EMPTY_BODY_HEIGHT = 32;
const PERSONALITY_PADDING = 16;
const SCHEMA_NODE_WIDTH = 200;
const SCHEMA_NODE_HEIGHT = 36;

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
const personalityNodeId = (id: string): string => `personality:${id}`;
const wakePortId = (instanceId: string, index: number): string =>
  `port:${instanceId}:${index}`;

export interface BuildModelInput {
  instances: PersonalityInstanceTs[];
}

export const computeLayout = async (
  input: BuildModelInput,
): Promise<CanvasModel> => {
  const triggerSchemas = new Set<string>();
  for (const instance of input.instances) {
    for (const entry of instance.wake_entries) {
      if (entry.trigger_id.trim() !== "") {
        triggerSchemas.add(entry.trigger_id);
      }
    }
  }

  const elkNodes: ElkNode[] = [];
  for (const schemaId of triggerSchemas) {
    elkNodes.push({
      id: schemaNodeId(schemaId),
      width: SCHEMA_NODE_WIDTH,
      height: SCHEMA_NODE_HEIGHT,
    });
  }
  for (const instance of input.instances) {
    elkNodes.push({
      id: personalityNodeId(instance.personality_instance_id),
      width: PERSONALITY_BASE_WIDTH,
      height: personalityHeight(instance),
      ports: instance.wake_entries.map((_, index) => ({
        id: wakePortId(instance.personality_instance_id, index),
        layoutOptions: { "port.side": "WEST" },
      })),
      layoutOptions: {
        portConstraints: "FIXED_ORDER",
      },
    });
  }

  const elkEdges: ElkNode["edges"] = [];
  let edgeCounter = 0;
  const sourceEdges: {
    source: string;
    target: string;
    schemaId: string;
    instanceId: string;
    entryIndex: number;
  }[] = [];

  for (const instance of input.instances) {
    instance.wake_entries.forEach((entry, index) => {
      const trigger = entry.trigger_id.trim();
      if (trigger === "" || !triggerSchemas.has(trigger)) return;
      const id = `edge:${edgeCounter++}`;
      const source = schemaNodeId(trigger);
      const target = wakePortId(
        instance.personality_instance_id,
        index,
      );
      elkEdges?.push({
        id,
        sources: [source],
        targets: [target],
      });
      sourceEdges.push({
        source,
        target,
        schemaId: trigger,
        instanceId: instance.personality_instance_id,
        entryIndex: index,
      });
    });
  }

  const elkGraph: ElkNode = {
    id: "personality-graph",
    layoutOptions,
    children: elkNodes,
    edges: elkEdges,
  };

  const positioned = await elk.layout(elkGraph);

  const nodes: CanvasNode[] = [];
  const nodeById = new Map<string, ElkNode>();
  for (const child of positioned.children ?? []) {
    nodeById.set(child.id, child);
  }

  for (const schemaId of triggerSchemas) {
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

  const edges: CanvasEdge[] = [];
  positioned.edges?.forEach((rawEdge, index) => {
    const meta = sourceEdges[index];
    if (!meta) return;
    const sections = rawEdge.sections ?? [];
    const path = sections
      .map((section) => {
        const parts: string[] = [];
        const start = section.startPoint;
        parts.push(`M ${start.x} ${start.y}`);
        for (const bend of section.bendPoints ?? []) {
          parts.push(`L ${bend.x} ${bend.y}`);
        }
        const end = section.endPoint;
        parts.push(`L ${end.x} ${end.y}`);
        return parts.join(" ");
      })
      .join(" ");
    edges.push({
      id: rawEdge.id,
      source: meta.source,
      target: meta.target,
      schema_id: meta.schemaId,
      src_instance_id: "",
      tgt_instance_id: meta.instanceId,
      tgt_entry_index: meta.entryIndex,
      path,
    });
  });

  return {
    nodes,
    edges,
    width: positioned.width ?? 0,
    height: positioned.height ?? 0,
  };
};
