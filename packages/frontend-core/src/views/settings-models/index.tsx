import { createResource, type Component } from "solid-js";
import { createTauriEngineClient } from "../../tauri-client";
import { sentinelOwner } from "../../graph-store";
import { EmbeddingSection } from "./embedding-section";
import { InferenceTargetsSection } from "./inference-targets-section";
import { loadActive, loadEmb } from "./loaders";

export const SettingsModelsPanel: Component = () => {
  const client = createTauriEngineClient();
  const owner = sentinelOwner();
  const [embeddingModels, { refetch: refetchEmb }] = createResource(loadEmb);
  const [active, { refetch: refetchActive }] = createResource(loadActive);
  const [targets, { refetch: refetchTargets }] = createResource(async () =>
    client.listInferenceTargets({ owner }),
  );
  const [bindings, { refetch: refetchBindings }] = createResource(async () =>
    client.listInferenceTierBindings({ owner }),
  );

  const refetchInferenceSettings = () => {
    refetchTargets();
    refetchBindings();
  };

  return (
    <div class="proxima-settings-panel">
      <InferenceTargetsSection
        client={client}
        owner={owner}
        targets={targets}
        refetchTargets={refetchTargets}
        bindings={bindings}
        refetchBindings={refetchBindings}
        onChanged={refetchInferenceSettings}
      />
      <EmbeddingSection
        embeddingModels={embeddingModels}
        active={active}
        onModelsChange={refetchEmb}
        onActiveChange={refetchActive}
      />
    </div>
  );
};
