import {
  commands,
  type EmbeddingModelRecord,
  type InferenceTargetTs,
  type InferenceTierBindingTs,
  type Owner,
} from "../../bindings";

// Loaders
export async function loadEmb(): Promise<EmbeddingModelRecord[]> {
  const r = await commands.modelsListEmbedding();
  if (r.status === "error") throw r.error;
  return r.data;
}

export async function loadInferenceTargets(
  owner: Owner,
): Promise<InferenceTargetTs[]> {
  const r = await commands.listInferenceTargets({ owner });
  if (r.status === "error") throw r.error;
  return r.data;
}

export async function loadInferenceTierBindings(
  owner: Owner,
): Promise<InferenceTierBindingTs[]> {
  const r = await commands.listInferenceTierBindings({ owner });
  if (r.status === "error") throw r.error;
  return r.data;
}

export async function loadActive(): Promise<{ vendor: string; model_id: string } | null> {
  const r = await commands.embeddingActiveGet();
  if (r.status === "error") throw r.error;
  return r.data;
}
