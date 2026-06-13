import {
  For,
  Show,
  createEffect,
  createMemo,
  createSignal,
  type Accessor,
  type Component,
} from "solid-js";
import type {
  InferenceTargetTs,
  InferenceTierBindingTs,
  ModelTierTs,
  Owner,
} from "../../bindings";
import type { EngineClient } from "../../client";
import {
  TIERS,
  configSummary,
  kindLabel,
} from "./constants";

interface Props {
  client: Pick<
    EngineClient,
    | "registerInferenceTarget"
    | "removeInferenceTarget"
    | "bindInferenceTier"
    | "inferenceEnvStatus"
    | "codexAuthStatus"
    | "testInferenceTarget"
  >;
  owner: Owner;
  targets: Accessor<InferenceTargetTs[] | undefined>;
  bindings: Accessor<InferenceTierBindingTs[] | undefined>;
  refetchTargets: () => void;
  refetchBindings: () => void;
}

interface CodexAuthState {
  auth_json_present: boolean;
  access_token_present: boolean;
}

const TIER_DESCRIPTIONS: Record<ModelTierTs, string> = {
  fast: "low-stakes wakes",
  standard: "most personality wakes",
  deep: "explicit deep-think wakes",
};

const errorMessage = (err: unknown): string => {
  if (err && typeof err === "object") {
    if ("code" in err && "message" in err) {
      return `${String((err as { code: unknown }).code)}: ${String((err as { message: unknown }).message)}`;
    }
    if ("message" in err && typeof (err as { message: unknown }).message === "string") {
      return (err as { message: string }).message;
    }
  }
  return String(err);
};

export const ModelsTable: Component<Props> = (props) => {
  const [error, setError] = createSignal<string | null>(null);

  const bindingFor = (tier: ModelTierTs): string | null =>
    (props.bindings() ?? []).find((b) => b.tier === tier)?.target_ref ?? null;

  const ownsTier = (targetRef: string): ModelTierTs[] =>
    TIERS.filter((tier) => bindingFor(tier) === targetRef);

  const handleTierClick = async (tier: ModelTierTs, targetRef: string) => {
    if (bindingFor(tier) === targetRef) return;
    setError(null);
    try {
      await props.client.bindInferenceTier({
        principal: props.owner.principal,
        tier,
        target_ref: targetRef,
      });
      props.refetchBindings();
    } catch (err) {
      setError(errorMessage(err));
    }
  };

  const handleRemove = async (targetRef: string) => {
    if (ownsTier(targetRef).length > 0) return;
    setError(null);
    try {
      await props.client.removeInferenceTarget({
        principal: props.owner.principal,
        target_ref: targetRef,
      });
      props.refetchTargets();
    } catch (err) {
      setError(errorMessage(err));
    }
  };

  const targets = createMemo(() => props.targets() ?? []);

  const [envStatus, setEnvStatus] = createSignal<Map<string, boolean>>(new Map());
  const [codexAuth, setCodexAuth] = createSignal<CodexAuthState | null>(null);

  const refreshEnvStatus = async () => {
    const seen = new Set<string>();
    const requests: Promise<[string, boolean]>[] = [];
    let needsCodex = false;
    for (const target of targets()) {
      if (target.config.kind === "chatgpt_codex") {
        needsCodex = true;
        continue;
      }
      if ("api_key_env" in target.config) {
        const key = target.config.api_key_env;
        if (key && !seen.has(key)) {
          seen.add(key);
          requests.push(
            props.client
              .inferenceEnvStatus({ env_var: key })
              .then((out) => [key, out.present] as [string, boolean])
              .catch(() => [key, false] as [string, boolean]),
          );
        }
      }
    }
    const results = await Promise.all(requests);
    setEnvStatus(new Map(results));
    if (needsCodex) {
      try {
        const out = await props.client.codexAuthStatus();
        setCodexAuth(out);
      } catch {
        setCodexAuth({ auth_json_present: false, access_token_present: false });
      }
    } else {
      setCodexAuth(null);
    }
  };

  createEffect(() => {
    void refreshEnvStatus();
  });

  interface TestOutcome {
    ok: boolean;
    latency_ms: number;
    error_code: string | null;
    error_message: string | null;
  }

  const [testResults, setTestResults] = createSignal<Map<string, TestOutcome>>(
    new Map(),
  );

  const handleTest = async (targetRef: string) => {
    setError(null);
    try {
      const out = await props.client.testInferenceTarget({
        principal: props.owner.principal,
        target_ref: targetRef,
      });
      setTestResults((prev) => {
        const next = new Map(prev);
        next.set(targetRef, out);
        return next;
      });
    } catch (err) {
      setError(errorMessage(err));
    }
  };

  return (
    <div class="proxima-models-table">
      <header class="proxima-tier-summary-header">
        <For each={TIERS}>
          {(tier) => (
            <div
              class="proxima-tier-summary-cell"
              data-testid={`tier-summary-${tier}`}
            >
              <span class="proxima-tier-summary-tier">{tier} →</span>{" "}
              <span class="proxima-tier-summary-target proxima-mono">
                {bindingFor(tier) ?? "(none)"}
              </span>
              <div class="proxima-tier-summary-desc proxima-dim">
                {TIER_DESCRIPTIONS[tier]}
              </div>
            </div>
          )}
        </For>
      </header>

      <Show when={error()}>
        {(message) => (
          <p class="proxima-error" role="alert">
            {message()}
          </p>
        )}
      </Show>

      <Show
        when={targets().length > 0}
        fallback={
          <div class="proxima-models-empty">
            <p class="proxima-dim">No models registered.</p>
            <p class="proxima-dim">
              Click <strong>+ Register model</strong> to add your first one.
            </p>
          </div>
        }
      >
        <table class="proxima-models-table-grid">
          <thead>
            <tr>
              <th>ref</th>
              <th>provider · model</th>
              <th>key</th>
              <For each={TIERS}>
                {(tier) => <th class="proxima-tier-radio-col">{tier[0]?.toUpperCase()}</th>}
              </For>
              <th></th>
            </tr>
          </thead>
          <tbody>
            <For each={targets()}>
              {(target) => (
                <tr>
                  <td>
                    <span class="proxima-mono">{target.target_ref}</span>
                  </td>
                  <td>
                    <div>{kindLabel(target.config.kind)}</div>
                    <div class="proxima-mono">{target.config.model_id}</div>
                    <div class="proxima-dim proxima-models-row-detail">
                      {configSummary(target.config)}
                    </div>
                  </td>
                  <td>
                    {(() => {
                      const config = target.config;
                      if (config.kind === "chatgpt_codex") {
                        const auth = codexAuth();
                        const ok =
                          auth?.auth_json_present === true &&
                          auth?.access_token_present === true;
                        const title = !auth
                          ? "checking ~/.codex/auth.json…"
                          : ok
                            ? "~/.codex/auth.json present with access_token"
                            : auth.auth_json_present
                              ? "~/.codex/auth.json present but missing tokens.access_token — run `codex login`"
                              : "~/.codex/auth.json not found — run `codex login`";
                        return (
                          <span
                            class={
                              "proxima-key-pill " +
                              (ok ? "is-set" : "is-missing")
                            }
                            aria-label={`codex auth status for ${target.target_ref}: ${ok ? "ready" : "missing"}`}
                            title={title}
                          >
                            {ok ? "● codex" : "○ codex"}
                          </span>
                        );
                      }
                      const key = "api_key_env" in config ? config.api_key_env : "";
                      const present = key ? envStatus().get(key) : undefined;
                      const label = present === true ? "set" : "missing";
                      return (
                        <span
                          class={
                            "proxima-key-pill " +
                            (present === true ? "is-set" : "is-missing")
                          }
                          aria-label={`key status for ${target.target_ref}: ${label}`}
                          title={key}
                        >
                          {present === true ? "● set" : "○ missing"}
                        </span>
                      );
                    })()}
                  </td>
                  <For each={TIERS}>
                    {(tier) => {
                      const checked = () => bindingFor(tier) === target.target_ref;
                      return (
                        <td class="proxima-tier-radio-col">
                          <input
                            type="radio"
                            aria-label={`bind tier ${tier} to ${target.target_ref}`}
                            checked={checked()}
                            onChange={() =>
                              void handleTierClick(tier, target.target_ref)
                            }
                          />
                        </td>
                      );
                    }}
                  </For>
                  <td>
                    <div class="proxima-models-row-actions">
                      <button
                        type="button"
                        class="proxima-btn"
                        aria-label={`test ${target.target_ref}`}
                        onClick={() => void handleTest(target.target_ref)}
                      >
                        test
                      </button>
                      <button
                        type="button"
                        class="proxima-btn proxima-btn-danger"
                        aria-label={`remove ${target.target_ref}`}
                        disabled={ownsTier(target.target_ref).length > 0}
                        title={
                          ownsTier(target.target_ref).length > 0
                            ? `reassign tier(s) ${ownsTier(target.target_ref).join(", ")} first`
                            : undefined
                        }
                        onClick={() => void handleRemove(target.target_ref)}
                      >
                        remove
                      </button>
                    </div>
                    <Show when={testResults().get(target.target_ref)}>
                      {(result) => (
                        <div
                          class={
                            "proxima-test-result " +
                            (result().ok ? "is-ok" : "is-err")
                          }
                        >
                          {result().ok ? (
                            <span>● tested ok · {result().latency_ms}ms</span>
                          ) : (
                            <span>
                              ○ failed · {result().error_code} ·{" "}
                              {result().error_message ?? "no detail"}
                            </span>
                          )}
                        </div>
                      )}
                    </Show>
                  </td>
                </tr>
              )}
            </For>
          </tbody>
        </table>
      </Show>
    </div>
  );
};
