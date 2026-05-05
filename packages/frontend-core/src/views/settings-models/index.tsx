import { createResource, type Component } from "solid-js";
import { EmbeddingSection } from "./embedding-section";
import { LlmSection } from "./llm-section";
import { TierBindingsSection } from "./tier-bindings-section";
import { loadActive, loadBindings, loadEmb, loadLlm } from "./loaders";

export const SettingsModelsPanel: Component = () => {
  const [llmModels, { refetch: refetchLlm }] = createResource(loadLlm);
  const [embeddingModels, { refetch: refetchEmb }] = createResource(loadEmb);
  const [bindings, { refetch: refetchBindings }] = createResource(loadBindings);
  const [active, { refetch: refetchActive }] = createResource(loadActive);

  return (
    <div class="proxima-settings-panel">
      <LlmSection llmModels={llmModels} onChange={refetchLlm} />
      <EmbeddingSection
        embeddingModels={embeddingModels}
        active={active}
        onModelsChange={refetchEmb}
        onActiveChange={refetchActive}
      />
      <TierBindingsSection
        bindings={bindings}
        llmModels={llmModels}
        onChange={refetchBindings}
      />
    </div>
  );
};
