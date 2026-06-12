import type {
  AuthoredByTs,
  ExecutionModeTs,
  ModelTierTs,
  PersonalityInstanceTs,
  ProducesTs,
  TriggerKindTs,
  WakeEntryDraftTs,
} from "../../bindings";

export type PersonalitySelection =
  | { kind: "personality"; instance_id: string }
  | { kind: "wake_entry"; instance_id: string; entry_index: number }
  | {
      kind: "edge";
      schema_id: string;
      tgt_instance_id: string;
      tgt_entry_index: number;
    }
  | null;

export type CanvasNodeKind = "personality" | "schema" | "relation";

export interface CanvasNode {
  id: string;
  kind: CanvasNodeKind;
  x: number;
  y: number;
  width: number;
  height: number;
  data:
    | { kind: "personality"; instance: PersonalityInstanceTs }
    | { kind: "schema"; schema_id: string }
    | { kind: "relation"; relation_id: string };
}

export interface TriggerCanvasEdge {
  kind: "trigger";
  id: string;
  source: string;
  target: string;
  shape_id: string; // schema_id (on_memory) or relation_id (on_edge)
  src_instance_id: ""; // unused for triggers; kept for shape uniformity
  tgt_instance_id: string;
  tgt_entry_index: number;
  path: string;
}

export interface ProducesCanvasEdge {
  kind: "produces";
  id: string;
  source: string;
  target: string;
  shape_id: string; // schema_id or relation_id
  src_instance_id: string;
  src_entry_index: number;
  tgt_instance_id: ""; // unused for produces
  tgt_entry_index: -1; // unused for produces
  path: string;
}

export type CanvasEdge = TriggerCanvasEdge | ProducesCanvasEdge;

export interface CanvasModel {
  nodes: CanvasNode[];
  edges: CanvasEdge[];
  width: number;
  height: number;
}

export const TRIGGER_KINDS: TriggerKindTs[] = ["on_memory", "on_edge"];
export const AUTHORED_BY: AuthoredByTs[] = ["any", "self_author", "other"];
export const EXECUTION_MODES: ExecutionModeTs[] = ["substrate_only"];
export const MODEL_TIERS: ModelTierTs[] = ["fast", "standard", "deep"];

export const emptyDraft = (
  triggerSchemaFallback: string,
): WakeEntryDraftTs => ({
  trigger_kind: "on_memory",
  trigger_id: triggerSchemaFallback,
  label: "",
  enabled: true,
  execution_mode: "substrate_only",
  authored_by: "any",
  probability_promille: 1000,
  goal_scope: "none",
  instructions: "",
  model_tier: "standard",
  inference_target_ref: null,
  substrate_tool_palette: [],
  required_produced_schema_ids: [],
  max_rounds: 16,
});

/** Map key: substrate-palette joined and sorted (`palette.slice().sort().join(',')`). */
export type ProducesByPaletteKey = Map<string, ProducesTs>;

export interface BuildModelInput {
  instances: PersonalityInstanceTs[];
  /**
   * Produces-set keyed by the canonical palette key for each entry's
   * substrate_tool_palette. Entries whose palette key is missing from
   * the map are treated as terminal (no produces edges).
   */
  producesByPaletteKey: ProducesByPaletteKey;
}

export const paletteKey = (substratePalette: string[]): string =>
  substratePalette.slice().sort().join(",");
