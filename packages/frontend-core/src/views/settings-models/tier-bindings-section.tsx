import {
  For,
  Show,
  createEffect,
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
  client: Pick<EngineClient, "bindInferenceTier">;
  targets: Accessor<InferenceTargetTs[] | undefined>;
  bindings: Accessor<InferenceTierBindingTs[] | undefined>;
  refetchBindings: () => void;
  owner?: Owner;
  embedded?: boolean;
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
  const [error, setError] = createSignal<string | null>(null);

  const handleBind = async (tier: ModelTierTs, targetRef: string) => {
    setError(null);
    try {
      await props.client.bindInferenceTier({
        owner: owner(),
        tier,
        target_ref: targetRef,
      });
      props.refetchBindings();
    } catch (err) {
      setError(errorMessage(err));
    }
  };

  const body = () => (
    <>
      <Show when={error()}>
        {(message) => <p class="proxima-error" role="alert">{message()}</p>}
      </Show>
      <table class="proxima-models-table">
        <thead>
          <tr>
            <th>Tier</th>
            <th>Target</th>
          </tr>
        </thead>
        <tbody>
          <For each={TIERS}>
            {(tier) => (
              <TierBindingRow
                tier={tier}
                targets={() => props.targets() ?? []}
                targetRef={() => bindingFor(props.bindings(), tier)}
                onBind={(selectedTier, targetRef) =>
                  void handleBind(selectedTier, targetRef)
                }
              />
            )}
          </For>
        </tbody>
      </table>
    </>
  );

  return (
    <Show when={!props.embedded} fallback={body()}>
      <section>
        <h2>Tier bindings</h2>
        {body()}
      </section>
    </Show>
  );
};
