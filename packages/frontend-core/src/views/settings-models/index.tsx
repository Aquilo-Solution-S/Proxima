import { createResource, type Component } from "solid-js";
import { createTauriEngineClient } from "../../tauri-client";
import { EmbeddingSection } from "./embedding-section";
import { InferenceTargetsSection } from "./inference-targets-section";
import { TierBindingsSection } from "./tier-bindings-section";
import { loadActive, loadEmb } from "./loaders";

export const SettingsModelsPanel: Component = () => {
  const client = createTauriEngineClient();
  const [embeddingModels, { refetch: refetchEmb }] = createResource(loadEmb);
  const [active, { refetch: refetchActive }] = createResource(loadActive);

  return (
    <div class="proxima-settings-panel">
      <InferenceTargetsSection client={client} />
      <TierBindingsSection client={client} />
      <EmbeddingSection
        embeddingModels={embeddingModels}
        active={active}
        onModelsChange={refetchEmb}
        onActiveChange={refetchActive}
      />
    </div>
  );
};
