import {
  For,
  Show,
  createResource,
  createSignal,
  type Component,
  type Resource,
} from "solid-js";
import {
  commands,
  type CommandError,
  type Dialect,
  type EmbedCaps,
  type EmbeddingModelRecord,
  type LlmCaps,
  type LlmModelRecord,
  type ModelTier,
  type TierBindings,
} from "../bindings";

const TIERS: ModelTier[] = ["fast", "standard", "deep"];
const DIALECTS: Dialect[] = ["anthropic", "openai"];

// Helper to format CommandError for display
function formatError(err: CommandError): string {
  switch (err.kind) {
    case "storage":
      return `Storage error: ${err.data.message}`;
    case "duplicate_llm_model":
      return `LLM model already registered: ${err.data.model_ref.vendor} / ${err.data.model_ref.model_id}`;
    case "duplicate_embedding_model":
      return `Embedding model already registered: ${err.data.model_ref.vendor} / ${err.data.model_ref.model_id}`;
    case "unknown_llm_model":
      return `Unknown LLM model: ${err.data.model_ref.vendor} / ${err.data.model_ref.model_id}`;
    case "unknown_embedding_model":
      return `Unknown embedding model: ${err.data.model_ref.vendor} / ${err.data.model_ref.model_id}`;
    case "insufficient_tier_caps":
      return `Model ${err.data.model_ref.vendor} / ${err.data.model_ref.model_id} doesn't satisfy ${err.data.tier} tier caps`;
    case "invariant":
      return `Internal invariant violation: ${err.data.message}`;
  }
}

// Caps badge helpers
const LlmCapsBadges: Component<{ caps: LlmCaps }> = (props) => {
  const axes = (): string[] => {
    const out: string[] = [];
    if (props.caps.tool_use) out.push("tool_use");
    if (props.caps.json_mode) out.push("json_mode");
    if (props.caps.long_context) out.push("long_context");
    if (props.caps.vision) out.push("vision");
    return out;
  };
  return (
    <span class="proxima-caps-badges">
      <For each={axes()} fallback={<span class="proxima-dim">—</span>}>
        {(axis) => <span class="proxima-caps-badge">{axis}</span>}
      </For>
    </span>
  );
};

const EmbedCapsBadges: Component<{ caps: EmbedCaps }> = (props) => (
  <span class="proxima-caps-badges">
    <span class="proxima-caps-badge">dim={props.caps.dim}</span>
    <Show when={props.caps.matryoshka}>
      <span class="proxima-caps-badge">matryoshka</span>
    </Show>
  </span>
);

// Loaders
async function loadLlm(): Promise<LlmModelRecord[]> {
  const r = await commands.modelsListLlm();
  if (r.status === "error") throw r.error;
  return r.data;
}

async function loadEmb(): Promise<EmbeddingModelRecord[]> {
  const r = await commands.modelsListEmbedding();
  if (r.status === "error") throw r.error;
  return r.data;
}

async function loadBindings(): Promise<TierBindings> {
  const r = await commands.tierBindingsGet();
  if (r.status === "error") throw r.error;
  return r.data;
}

async function loadActive(): Promise<{ vendor: string; model_id: string } | null> {
  const r = await commands.embeddingActiveGet();
  if (r.status === "error") throw r.error;
  return r.data;
}

async function loadTierRequires(tier: ModelTier): Promise<LlmCaps> {
  const r = await commands.tierRequires(tier);
  if (r.status === "error") throw r.error;
  return r.data;
}

// LLM Section
const LlmSection: Component<{
  llmModels: Resource<LlmModelRecord[]>;
  onChange: () => void;
}> = (props) => {
  const [error, setError] = createSignal<string | null>(null);
  const [formData, setFormData] = createSignal({
    vendor: "",
    model_id: "",
    dialect: "anthropic" as Dialect,
    base_url: "",
    tool_use: false,
    json_mode: false,
    long_context: false,
    vision: false,
    secret_ref: "",
  });

  const handleSubmit = async (e: Event) => {
    e.preventDefault();
    setError(null);
    const data = formData();
    const record: LlmModelRecord = {
      vendor: data.vendor,
      model_id: data.model_id,
      dialect: data.dialect,
      base_url: data.base_url,
      caps: {
        tool_use: data.tool_use,
        json_mode: data.json_mode,
        long_context: data.long_context,
        vision: data.vision,
      },
      secret_ref: data.secret_ref || null,
    };
    const r = await commands.modelsRegisterLlm(record);
    if (r.status === "error") {
      setError(formatError(r.error));
    } else {
      setFormData({
        vendor: "",
        model_id: "",
        dialect: "anthropic",
        base_url: "",
        tool_use: false,
        json_mode: false,
        long_context: false,
        vision: false,
        secret_ref: "",
      });
      props.onChange();
    }
  };

  const handleDelete = async (vendor: string, modelId: string) => {
    const r = await commands.modelsDeleteLlm(vendor, modelId);
    if (r.status === "error") {
      setError(formatError(r.error));
    } else {
      props.onChange();
    }
  };

  return (
    <section>
      <h2>LLM models</h2>
      <Show when={props.llmModels.error}>
        <p class="proxima-error">Error: {formatError(props.llmModels.error!)}</p>
      </Show>
      <Show when={props.llmModels.loading}>
        <p class="proxima-dim">Loading…</p>
      </Show>
      <Show when={props.llmModels()}>
        {(models) => (
          <>
            <Show when={models().length === 0}>
              <p class="proxima-dim">No LLM models registered.</p>
            </Show>
            <Show when={models().length > 0}>
              <table class="proxima-models-table">
                <thead>
                  <tr>
                    <th>Vendor</th>
                    <th>Model ID</th>
                    <th>Dialect</th>
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
                          <span class="proxima-mono">{m.dialect}</span>
                        </td>
                        <td>
                          <span class="proxima-mono">{m.base_url}</span>
                        </td>
                        <td>
                          <LlmCapsBadges caps={m.caps} />
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
            <label for="llm-vendor">Vendor</label>
            <input
              id="llm-vendor"
              type="text"
              value={formData().vendor}
              onInput={(e) =>
                setFormData({ ...formData(), vendor: e.target.value })
              }
              placeholder="anthropic"
            />

            <label for="llm-model-id">Model ID</label>
            <input
              id="llm-model-id"
              type="text"
              value={formData().model_id}
              onInput={(e) =>
                setFormData({ ...formData(), model_id: e.target.value })
              }
              placeholder="claude-3-7-sonnet-20250219"
            />

            <label for="llm-dialect">Dialect</label>
            <select
              id="llm-dialect"
              value={formData().dialect}
              onChange={(e) =>
                setFormData({ ...formData(), dialect: e.target.value as Dialect })
              }
            >
              <For each={DIALECTS}>
                {(d) => <option value={d}>{d}</option>}
              </For>
            </select>

            <label for="llm-base-url">Base URL</label>
            <input
              id="llm-base-url"
              type="text"
              value={formData().base_url}
              onInput={(e) =>
                setFormData({ ...formData(), base_url: e.target.value })
              }
              placeholder="https://api.anthropic.com"
            />

            <label for="llm-secret-ref">Secret Ref</label>
            <input
              id="llm-secret-ref"
              type="text"
              value={formData().secret_ref}
              onInput={(e) =>
                setFormData({ ...formData(), secret_ref: e.target.value })
              }
              placeholder="keychain://anthropic_api_key"
            />

            <label>Capabilities</label>
            <div class="checkbox-row">
              <label>
                <input
                  type="checkbox"
                  checked={formData().tool_use}
                  onChange={(e) =>
                    setFormData({ ...formData(), tool_use: e.target.checked })
                  }
                />
                tool_use
              </label>
              <label>
                <input
                  type="checkbox"
                  checked={formData().json_mode}
                  onChange={(e) =>
                    setFormData({ ...formData(), json_mode: e.target.checked })
                  }
                />
                json_mode
              </label>
              <label>
                <input
                  type="checkbox"
                  checked={formData().long_context}
                  onChange={(e) =>
                    setFormData({ ...formData(), long_context: e.target.checked })
                  }
                />
                long_context
              </label>
              <label>
                <input
                  type="checkbox"
                  checked={formData().vision}
                  onChange={(e) =>
                    setFormData({ ...formData(), vision: e.target.checked })
                  }
                />
                vision
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

// Embedding Section
const EmbeddingSection: Component<{
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
        <p class="proxima-dim">Loading…</p>
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
        <p class="proxima-dim">Loading…</p>
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
              placeholder="keychain://openai_api_key"
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

// Tier Bindings Section
const TierBindingsSection: Component<{
  bindings: Resource<TierBindings>;
  llmModels: Resource<LlmModelRecord[]>;
  onChange: () => void;
}> = (props) => {
  const [error, setError] = createSignal<string | null>(null);

  // Create resources for tier requirements
  const [fastReq] = createResource(() => loadTierRequires("fast"));
  const [standardReq] = createResource(() => loadTierRequires("standard"));
  const [deepReq] = createResource(() => loadTierRequires("deep"));

  const handleBind = async (tier: ModelTier, vendor: string, modelId: string) => {
    const r = await commands.tierBind(tier, vendor, modelId);
    if (r.status === "error") {
      setError(formatError(r.error));
    } else {
      props.onChange();
    }
  };

  const handleUnbind = async (tier: ModelTier) => {
    const r = await commands.tierUnbind(tier);
    if (r.status === "error") {
      setError(formatError(r.error));
    } else {
      props.onChange();
    }
  };

  const formatCaps = (caps: LlmCaps): string => {
    const parts: string[] = [];
    if (caps.tool_use) parts.push("tool_use");
    if (caps.json_mode) parts.push("json_mode");
    if (caps.long_context) parts.push("long_context");
    if (caps.vision) parts.push("vision");
    return parts.join(", ");
  };

  return (
    <section>
      <h2>Tier bindings</h2>
      <Show when={error()}>
        <p class="proxima-error">{error()}</p>
      </Show>
      <Show when={props.bindings()}>
        {(bindings) => (
          <For each={TIERS}>
            {(tier) => {
              const binding = () => bindings()?.[tier];
              const models = () => props.llmModels() ?? [];
              const reqResource =
                tier === "fast"
                  ? fastReq
                  : tier === "standard"
                    ? standardReq
                    : deepReq;

              return (
                <div>
                  <Show when={reqResource()}>
                    {(reqCaps) => (
                      <p class="proxima-tier-requires">
                        requires: {formatCaps(reqCaps())}
                      </p>
                    )}
                  </Show>
                  <div class="proxima-tier-row">
                    <span class="proxima-tier-label">{tier}</span>
                    <select
                      value={binding() ? `${binding()!.vendor}|${binding()!.model_id}` : ""}
                      onChange={(e) => {
                        if (e.target.value) {
                          const [vendor, modelId] = e.target.value.split("|");
                          void handleBind(tier, vendor, modelId);
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
                      onClick={() => void handleUnbind(tier)}
                      disabled={!binding()}
                    >
                      Unbind
                    </button>
                  </div>
                </div>
              );
            }}
          </For>
        )}
      </Show>
    </section>
  );
};

// Main panel component
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
