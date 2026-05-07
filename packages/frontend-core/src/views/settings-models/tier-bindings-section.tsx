import {
  For,
  Show,
  createEffect,
  createResource,
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
import { sentinelOwner } from "../../graph-store";
import { TIERS } from "./constants";

interface Props {
  client: Pick<
    EngineClient,
    "listInferenceTargets" | "listInferenceTierBindings" | "bindInferenceTier"
  >;
  owner?: Owner;
}

const errorMessage = (err: unknown): string => {
  if (err && typeof err === "object") {
    if ("code" in err && "message" in err) {
      const code = (err as { code: unknown }).code;
      const message = (err as { message: unknown }).message;
      return `${String(code)}: ${String(message)}`;
    }
    if ("message" in err) {
      const message = (err as { message: unknown }).message;
      if (typeof message === "string") return message;
    }
  }
  return String(err);
};

const bindingFor = (
  bindings: InferenceTierBindingTs[] | undefined,
  tier: ModelTierTs,
): string | undefined =>
  bindings?.find((binding) => binding.tier === tier)?.target_ref;

const TierBindingRow: Component<{
  tier: ModelTierTs;
  targets: Accessor<InferenceTargetTs[]>;
  targetRef: Accessor<string | undefined>;
  onBind: (tier: ModelTierTs, targetRef: string) => void;
}> = (props) => {
  let select: HTMLSelectElement | undefined;

  createEffect(() => {
    const value = props.targetRef() ?? "";
    props.targets();
    queueMicrotask(() => {
      if (select) select.value = value;
    });
  });

  return (
    <tr>
      <td>{props.tier}</td>
      <td>
        <select
          ref={select}
          value={props.targetRef() ?? ""}
          onChange={(event) => {
            const targetRef = event.currentTarget.value;
            if (targetRef) props.onBind(props.tier, targetRef);
          }}
        >
          <option value="">(none)</option>
          <For each={props.targets()}>
            {(target) => (
              <option
                value={target.target_ref}
                selected={props.targetRef() === target.target_ref}
              >
                {target.target_ref}
              </option>
            )}
          </For>
        </select>
      </td>
    </tr>
  );
};

export const TierBindingsSection: Component<Props> = (props) => {
  const owner = () => props.owner ?? sentinelOwner();
  const [targets] = createResource(async () =>
    props.client.listInferenceTargets({ owner: owner() }),
  );
  const [bindings, { refetch }] = createResource(async () =>
    props.client.listInferenceTierBindings({ owner: owner() }),
  );
  const [error, setError] = createSignal<string | null>(null);

  const handleBind = async (tier: ModelTierTs, targetRef: string) => {
    setError(null);
    try {
      await props.client.bindInferenceTier({
        owner: owner(),
        tier,
        target_ref: targetRef,
      });
      refetch();
    } catch (err) {
      setError(errorMessage(err));
    }
  };

  return (
    <section>
      <h2>Tier bindings</h2>
      <Show when={error()}>
        {(message) => <p class="proxima-error" role="alert">{message()}</p>}
      </Show>
      <table class="proxima-models-table">
        <thead>
          <tr>
            <th>tier</th>
            <th>target_ref</th>
          </tr>
        </thead>
        <tbody>
          <For each={TIERS}>
            {(tier) => (
              <TierBindingRow
                tier={tier}
                targets={() => targets() ?? []}
                targetRef={() => bindingFor(bindings(), tier)}
                onBind={(selectedTier, targetRef) =>
                  void handleBind(selectedTier, targetRef)
                }
              />
            )}
          </For>
        </tbody>
      </table>
    </section>
  );
};
