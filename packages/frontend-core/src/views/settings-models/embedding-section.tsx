import { For, Show, createSignal, type Component, type Resource } from "solid-js";
import { commands, type EmbeddingModelRecord } from "../../bindings";
import { formatCommandError as formatError } from "../../format-error";
import { LoadingSurface } from "../../primitives";
import { EmbedCapsBadges } from "./badges";

// Embedding Section
export const EmbeddingSection: Component<{
  embeddingModels: Resource<EmbeddingModelRecord[]>;
  active: Resource<{ vendor: string; model_id: string } | null>;
  onModelsChange: () => void;
  onActiveChange: () => void;
}> = (props) => {
  const [error, setError] = createSignal<string | null>(null);
  const [formData, setFormData] = createSignal({
    vendor: "",
    model_id: "",
    base_url: "",
    dim: 0,
    matryoshka: false,
    secret_ref: "",
  });

  const handleSubmit = async (e: Event) => {
    e.preventDefault();
    setError(null);
    const data = formData();
    if (data.dim <= 0) {
      setError("dim must be a positive integer");
      return;
    }
    const record: EmbeddingModelRecord = {
      vendor: data.vendor,
      model_id: data.model_id,
      base_url: data.base_url,
      caps: {
        dim: data.dim,
        matryoshka: data.matryoshka,
      },
      secret_ref: data.secret_ref || null,
    };
    const r = await commands.modelsRegisterEmbedding(record);
    if (r.status === "error") {
      setError(formatError(r.error));
    } else {
      setFormData({
        vendor: "",
        model_id: "",
        base_url: "",
        dim: 0,
        matryoshka: false,
        secret_ref: "",
      });
      props.onModelsChange();
    }
  };

  const handleDelete = async (vendor: string, modelId: string) => {
    const r = await commands.modelsDeleteEmbedding(vendor, modelId);
    if (r.status === "error") {
      setError(formatError(r.error));
    } else {
      props.onModelsChange();
    }
  };

  const handleSetActive = async (vendor: string, modelId: string) => {
    const r = await commands.embeddingActiveSet(vendor, modelId);
    if (r.status === "error") {
      setError(formatError(r.error));
    } else {
      props.onActiveChange();
    }
  };

  const handleClearActive = async () => {
    const r = await commands.embeddingActiveClear();
    if (r.status === "error") {
      setError(formatError(r.error));
    } else {
      props.onActiveChange();
    }
  };

  return (
    <section>
      <h2>Embedding</h2>

      <h3>Active model</h3>
      <Show when={props.active.error}>
        <p class="proxima-error">Error: {formatError(props.active.error!)}</p>
      </Show>
      <Show when={props.active.loading}>
        <LoadingSurface mode="inline" label="Loading" size={36} />
      </Show>
      <Show when={props.active()}>
        {(active) => (
          <p class="proxima-dim">
            Current: {active() ? (
              <>
                <span class="proxima-mono">{active()!.vendor}</span> / {
                  <span class="proxima-mono">{active()!.model_id}</span>
                }
              </>
            ) : (
              <span class="proxima-dim">(none)</span>
            )}
          </p>
        )}
      </Show>
      <Show when={props.embeddingModels()}>
        {(models) => (
          <div class="proxima-embedding-active-row">
            <select
              value={props.active() ? `${props.active()!.vendor}|${props.active()!.model_id}` : ""}
              onChange={(e) => {
                const [vendor, modelId] = e.target.value.split("|");
                if (vendor && modelId) {
                  void handleSetActive(vendor, modelId);
                }
              }}
            >
              <option value="">— Select model —</option>
              <For each={models()}>
                {(m) => (
                  <option value={`${m.vendor}|${m.model_id}`}>
                    {m.vendor} / {m.model_id}
                  </option>
                )}
              </For>
            </select>
            <button
              class="proxima-btn proxima-btn-danger"
              onClick={() => void handleClearActive()}
              disabled={!props.active()}
            >
              Clear
            </button>
          </div>
        )}
      </Show>

      <h3>Registered models</h3>
      <Show when={props.embeddingModels.error}>
        <p class="proxima-error">Error: {formatError(props.embeddingModels.error!)}</p>
      </Show>
      <Show when={props.embeddingModels.loading}>
        <LoadingSurface mode="inline" label="Loading" size={36} />
      </Show>
      <Show when={props.embeddingModels()}>
        {(models) => (
          <>
            <Show when={models().length === 0}>
              <p class="proxima-dim">No embedding models registered.</p>
            </Show>
            <Show when={models().length > 0}>
              <table class="proxima-models-table">
                <thead>
                  <tr>
                    <th>Vendor</th>
                    <th>Model ID</th>
                    <th>Base URL</th>
                    <th>Caps</th>
                    <th></th>
                  </tr>
                </thead>
                <tbody>
                  <For each={models()}>
                    {(m) => (
                      <tr>
                        <td>
                          <span class="proxima-mono">{m.vendor}</span>
                        </td>
                        <td>
                          <span class="proxima-mono">{m.model_id}</span>
                        </td>
                        <td>
                          <span class="proxima-mono">{m.base_url}</span>
                        </td>
                        <td>
                          <EmbedCapsBadges caps={m.caps} />
                        </td>
                        <td>
                          <button
                            class="proxima-btn proxima-btn-danger"
                            onClick={() => handleDelete(m.vendor, m.model_id)}
                          >
                            Delete
                          </button>
                        </td>
                      </tr>
                    )}
                  </For>
                </tbody>
              </table>
            </Show>
          </>
        )}
      </Show>

      <details class="proxima-models-form">
        <summary>Register model</summary>
        <form onSubmit={handleSubmit}>
          <Show when={error()}>
            <p class="proxima-error">{error()}</p>
          </Show>
          <div class="proxima-models-form-grid">
            <label for="emb-vendor">Vendor</label>
            <input
              id="emb-vendor"
              type="text"
              value={formData().vendor}
              onInput={(e) =>
                setFormData({ ...formData(), vendor: e.target.value })
              }
              placeholder="text-embeddings-3-small"
            />

            <label for="emb-model-id">Model ID</label>
            <input
              id="emb-model-id"
              type="text"
              value={formData().model_id}
              onInput={(e) =>
                setFormData({ ...formData(), model_id: e.target.value })
              }
              placeholder="text-embeddings-3-small"
            />

            <label for="emb-base-url">Base URL</label>
            <input
              id="emb-base-url"
              type="text"
              value={formData().base_url}
              onInput={(e) =>
                setFormData({ ...formData(), base_url: e.target.value })
              }
              placeholder="https://api.openai.com/v1"
            />

            <label for="emb-dim">Dim</label>
            <input
              id="emb-dim"
              type="number"
              value={formData().dim}
              onInput={(e) =>
                setFormData({ ...formData(), dim: parseInt(e.target.value, 10) || 0 })
              }
              min="1"
              placeholder="1536"
            />

            <label for="emb-secret-ref">Secret Ref</label>
            <input
              id="emb-secret-ref"
              type="text"
              value={formData().secret_ref}
              onInput={(e) =>
                setFormData({ ...formData(), secret_ref: e.target.value })
              }
              placeholder="keychain:proxima:openai_api_key"
            />

            <label>Capabilities</label>
            <div class="checkbox-row">
              <label>
                <input
                  type="checkbox"
                  checked={formData().matryoshka}
                  onChange={(e) =>
                    setFormData({ ...formData(), matryoshka: e.target.checked })
                  }
                />
                matryoshka
              </label>
            </div>
          </div>
          <div class="proxima-models-form-actions">
            <button type="submit" class="proxima-btn">
              Register
            </button>
          </div>
        </form>
      </details>
    </section>
  );
};
