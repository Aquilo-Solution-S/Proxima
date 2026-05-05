import { commands, type EmbeddingModelRecord, type LlmCaps, type LlmModelRecord, type ModelTier, type TierBindings } from "../../bindings";

// Loaders
export async function loadLlm(): Promise<LlmModelRecord[]> {
  const r = await commands.modelsListLlm();
  if (r.status === "error") throw r.error;
  return r.data;
}

export async function loadEmb(): Promise<EmbeddingModelRecord[]> {
  const r = await commands.modelsListEmbedding();
  if (r.status === "error") throw r.error;
  return r.data;
}

export async function loadBindings(): Promise<TierBindings> {
  const r = await commands.tierBindingsGet();
  if (r.status === "error") throw r.error;
  return r.data;
}

export async function loadActive(): Promise<{ vendor: string; model_id: string } | null> {
  const r = await commands.embeddingActiveGet();
  if (r.status === "error") throw r.error;
  return r.data;
}

export async function loadTierRequires(tier: ModelTier): Promise<LlmCaps> {
  const r = await commands.tierRequires(tier);
  if (r.status === "error") throw r.error;
  return r.data;
}
