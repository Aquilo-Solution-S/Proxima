import type {
  AuthoredByTs,
  ExecutionModeTs,
  ModelTierTs,
  PersonalityInstanceTs,
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

export type CanvasNodeKind = "personality" | "schema";

export interface CanvasNode {
  id: string;
  kind: CanvasNodeKind;
  x: number;
  y: number;
  width: number;
  height: number;
  data:
    | { kind: "personality"; instance: PersonalityInstanceTs }
    | { kind: "schema"; schema_id: string };
}

export interface CanvasEdge {
  id: string;
  source: string;
  target: string;
  schema_id: string;
  src_instance_id: string;
  tgt_instance_id: string;
  tgt_entry_index: number;
  path: string;
}

export interface CanvasModel {
  nodes: CanvasNode[];
  edges: CanvasEdge[];
  width: number;
  height: number;
}

export const TRIGGER_KINDS: TriggerKindTs[] = ["on_memory", "on_edge"];
export const AUTHORED_BY: AuthoredByTs[] = ["any", "self_author", "other"];
export const EXECUTION_MODES: ExecutionModeTs[] = [
  "substrate_only",
  "workspace",
];
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
  recipe_ref: "",
  model_tier: "standard",
  inference_target_ref: null,
  substrate_tool_palette: [],
  workspace_tool_palette: [],
  max_rounds: 4,
});
