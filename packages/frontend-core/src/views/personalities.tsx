import "./personalities.css";

import {
  For,
  Show,
  createEffect,
  createMemo,
  createSignal,
  type Component,
} from "solid-js";
import {
  commands,
  type InstantiatePersonalityOutcomeTs,
  type InstantiatePersonalityTs,
  type ListPersonalityInstancesTs,
  type Owner,
  type PersonalityInstanceTs,
  type ProtocolError,
  type SetWakeConfigOutcomeTs,
  type SetWakeConfigTs,
  type WakeFilterTs,
} from "../bindings";
import { sentinelOwner } from "../graph-store";
import { Mono } from "../primitives";

const ENGINEER_TYPE = "proxima-code/engineer-v1";

type CommandResult<T> = Promise<
  { status: "ok"; data: T } | { status: "error"; error: ProtocolError }
>;

export type PersonalityCommandClient = {
  provisionOwner: (owner: Owner) => CommandResult<null>;
  listPersonalityInstances: (
    req: ListPersonalityInstancesTs,
  ) => CommandResult<PersonalityInstanceTs[]>;
  instantiatePersonality: (
    req: InstantiatePersonalityTs,
  ) => CommandResult<InstantiatePersonalityOutcomeTs>;
  setWakeConfig: (req: SetWakeConfigTs) => CommandResult<SetWakeConfigOutcomeTs>;
};

const emptyOnMemory = (): WakeFilterTs => ({
  kind: "on_memory",
  version: 1,
  schema_id: "proxima-code/commit-summary-v1",
  authored_by: { kind: "any" },
  probability: 1,
});

const emptyOnEdge = (): WakeFilterTs => ({
  kind: "on_edge",
  version: 1,
  relation_id: "core/inspires",
  source: { kind: "any" },
  target: { kind: "self_perspective" },
  probability: 1,
});

export const EngineerInstancesPanel: Component<{
  client?: PersonalityCommandClient;
  owner?: Owner;
}> = (props) => {
  const owner = props.owner ?? sentinelOwner();
  const client = props.client ?? commands;
  const [instances, setInstances] = createSignal<PersonalityInstanceTs[]>([]);
  const [loading, setLoading] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [displayName, setDisplayName] = createSignal("Engineer");
  const [purpose, setPurpose] = createSignal("Develop perspectives on code changes");
  const [editing, setEditing] = createSignal<PersonalityInstanceTs | null>(null);

  const engineers = createMemo(() =>
    instances().filter((row) => row.personality_type_id === ENGINEER_TYPE),
  );
  const others = createMemo(() =>
    instances().filter((row) => row.personality_type_id !== ENGINEER_TYPE),
  );

  const refresh = async () => {
    setLoading(true);
    setError(null);
    try {
      await unwrap(client.provisionOwner(owner));
      const rows = await unwrap(
        client.listPersonalityInstances({
          owner,
          personality_type_id: null,
        }),
      );
      setInstances(rows);
    } catch (err) {
      setError(String(err));
    } finally {
      setLoading(false);
    }
  };

  createEffect(() => {
    void refresh();
  });

  const createEngineer = async () => {
    setLoading(true);
    setError(null);
    try {
      await unwrap(
        client.instantiatePersonality({
          owner,
          personality_type_id: ENGINEER_TYPE,
          payload_overrides: JSON.stringify({
            display_name: displayName(),
            purpose: purpose(),
          }),
        }),
      );
      await refresh();
    } catch (err) {
      setError(String(err));
      setLoading(false);
    }
  };

  return (
    <section class="personality-view">
      <div class="personality-toolbar">
        <div>
          <h1>Personalities</h1>
          <Show when={error()}>
            {(message) => <p class="proxima-error">{message()}</p>}
          </Show>
        </div>
        <div class="personality-actions">
          <button type="button" class="hub-nav-item" onClick={() => void refresh()}>
            Refresh
          </button>
        </div>
      </div>

      <div class="personality-form">
        <label>
          Display name
          <input
            value={displayName()}
            onInput={(event) => setDisplayName(event.currentTarget.value)}
          />
        </label>
        <label>
          Purpose
          <input
            value={purpose()}
            onInput={(event) => setPurpose(event.currentTarget.value)}
          />
        </label>
        <button
          type="button"
          class="hub-nav-item"
          disabled={loading()}
          onClick={() => void createEngineer()}
        >
          Create another Engineer
        </button>
      </div>

      <div class="personality-grid" aria-busy={loading()}>
        <For each={[...engineers(), ...others()]}>
          {(instance) => (
            <PersonalityCard instance={instance} onEdit={() => setEditing(instance)} />
          )}
        </For>
      </div>

      <Show when={editing()}>
        {(instance) => (
          <WakeEditor
            instance={instance()}
            onClose={() => setEditing(null)}
            onSaved={() => {
              setEditing(null);
              void refresh();
            }}
            client={client}
          />
        )}
      </Show>
    </section>
  );
};

const PersonalityCard: Component<{
  instance: PersonalityInstanceTs;
  onEdit: () => void;
}> = (props) => {
  const statusClass = () => props.instance.status.replace(/-/g, "_");
  return (
    <article class="personality-row" data-testid="personality-card">
      <div class="personality-row-head">
        <div class="personality-title">
          <strong>{props.instance.display_name}</strong>
          <div class="personality-meta">
            <span>{props.instance.personality_type_id}</span>
            <Mono>{shortId(props.instance.personality_instance_id)}</Mono>
          </div>
        </div>
        <span class={`personality-status ${statusClass()}`}>
          {props.instance.status}
        </span>
      </div>
      <div class="self-perspective-ref">
        <span>Self</span>
        <button
          type="button"
          class="self-perspective-button"
          title={props.instance.current_self_perspective_memory_id}
          onClick={() => void copyText(props.instance.current_self_perspective_memory_id)}
        >
          <Mono>{shortId(props.instance.current_self_perspective_memory_id)}</Mono>
        </button>
      </div>
      <Show when={props.instance.status === "needs_repair"}>
        <div class="repair-banner">
          <span>
            Wake config needs repair - saved filters could not be loaded after a
            recent update.
          </span>
          <button type="button" class="hub-nav-item" onClick={props.onEdit}>
            Re-edit
          </button>
        </div>
      </Show>
      <ul class="wake-list">
        <For each={props.instance.wake_filters}>
          {(filter) => <li class="wake-item">{wakeSummary(filter)}</li>}
        </For>
      </ul>
      <button type="button" class="hub-nav-item" onClick={props.onEdit}>
        Edit wake config
      </button>
    </article>
  );
};

const WakeEditor: Component<{
  instance: PersonalityInstanceTs;
  onClose: () => void;
  onSaved: () => void;
  client: PersonalityCommandClient;
}> = (props) => {
  const [filters, setFilters] = createSignal<WakeFilterTs[]>(
    props.instance.status === "needs_repair"
      ? []
      : props.instance.wake_filters.map(cloneFilter),
  );
  const [error, setError] = createSignal<string | null>(null);
  const [saving, setSaving] = createSignal(false);

  const updateFilter = (index: number, next: WakeFilterTs) => {
    setFilters((prev) => prev.map((filter, i) => (i === index ? next : filter)));
  };

  const save = async () => {
    setSaving(true);
    setError(null);
    try {
      await unwrap(
        props.client.setWakeConfig({
          owner: props.instance.owner,
          personality_type_id: props.instance.personality_type_id,
          personality_instance_id: props.instance.personality_instance_id,
          wake_filters: filters(),
        }),
      );
      props.onSaved();
    } catch (err) {
      setError(String(err));
    } finally {
      setSaving(false);
    }
  };

  return (
    <div class="wake-editor-backdrop" role="dialog" aria-modal="true">
      <div class="wake-editor">
        <div class="wake-editor-head">
          <div>
            <h2>{props.instance.display_name}</h2>
            <Mono>{props.instance.personality_instance_id}</Mono>
            <Show when={props.instance.status === "needs_repair"}>
              <p class="proxima-error">wake config needs repair</p>
            </Show>
            <Show when={error()}>
              {(message) => <p class="proxima-error">{message()}</p>}
            </Show>
          </div>
          <button type="button" class="hub-nav-item" onClick={props.onClose}>
            Close
          </button>
        </div>

        <div class="wake-editor-list" data-testid="wake-filters-list">
          <For each={filters()}>
            {(filter, index) => (
              <WakeFilterEditor
                filter={filter}
                onChange={(next) => updateFilter(index(), next)}
                onRemove={() =>
                  setFilters((prev) => prev.filter((_, i) => i !== index()))
                }
              />
            )}
          </For>
        </div>

        <div class="wake-editor-actions">
          <div class="personality-actions">
            <button
              type="button"
              class="hub-nav-item"
              onClick={() => setFilters((prev) => [...prev, emptyOnMemory()])}
            >
              Add OnMemory
            </button>
            <button
              type="button"
              class="hub-nav-item"
              onClick={() => setFilters((prev) => [...prev, emptyOnEdge()])}
            >
              Add OnEdge
            </button>
          </div>
          <button
            type="button"
            class="hub-nav-item"
            disabled={saving()}
            onClick={() => void save()}
          >
            Save
          </button>
        </div>
      </div>
    </div>
  );
};

const WakeFilterEditor: Component<{
  filter: WakeFilterTs;
  onChange: (next: WakeFilterTs) => void;
  onRemove: () => void;
}> = (props) => {
  const probability = () => String(props.filter.probability);
  const changeProbability = (value: string) => {
    const probability = Number(value);
    if (Number.isFinite(probability)) {
      props.onChange({ ...props.filter, probability });
    }
  };

  return (
    <div class="wake-editor-row">
      <label>
        Kind
        <select
          value={props.filter.kind}
          onChange={(event) =>
            props.onChange(
              event.currentTarget.value === "on_edge"
                ? emptyOnEdge()
                : emptyOnMemory(),
            )
          }
        >
          <option value="on_memory">OnMemory</option>
          <option value="on_edge">OnEdge</option>
        </select>
      </label>
      <Show
        when={props.filter.kind === "on_memory"}
        fallback={<OnEdgeFields filter={props.filter} onChange={props.onChange} />}
      >
        <OnMemoryFields filter={props.filter} onChange={props.onChange} />
      </Show>
      <label>
        Probability
        <input
          type="number"
          min="0"
          max="1"
          step="0.001"
          value={probability()}
          onInput={(event) => changeProbability(event.currentTarget.value)}
        />
      </label>
      <button type="button" class="hub-nav-item" onClick={props.onRemove}>
        Remove
      </button>
    </div>
  );
};

const OnMemoryFields: Component<{
  filter: WakeFilterTs;
  onChange: (next: WakeFilterTs) => void;
}> = (props) => {
  const filter = props.filter;
  if (filter.kind !== "on_memory") return null;
  return (
    <>
      <label>
        Schema
        <input
          value={filter.schema_id}
          onInput={(event) =>
            props.onChange({ ...filter, schema_id: event.currentTarget.value })
          }
        />
      </label>
      <label>
        Author
        <select
          value={filter.authored_by.kind}
          onChange={(event) =>
            props.onChange({
              ...filter,
              authored_by:
                event.currentTarget.value === "external"
                  ? { kind: "external" }
                  : { kind: "any" },
            })
          }
        >
          <option value="any">Any</option>
          <option value="external">External</option>
        </select>
      </label>
    </>
  );
};

const OnEdgeFields: Component<{
  filter: WakeFilterTs;
  onChange: (next: WakeFilterTs) => void;
}> = (props) => {
  const filter = props.filter;
  if (filter.kind !== "on_edge") return null;
  return (
    <>
      <label>
        Relation
        <input
          value={filter.relation_id}
          onInput={(event) =>
            props.onChange({
              ...filter,
              relation_id: event.currentTarget.value,
            })
          }
        />
      </label>
      <label>
        Target
        <select
          value={filter.target.kind}
          onChange={(event) =>
            props.onChange({
              ...filter,
              target:
                event.currentTarget.value === "self_perspective"
                  ? { kind: "self_perspective" }
                  : { kind: "any" },
            })
          }
        >
          <option value="self_perspective">Self</option>
          <option value="any">Any</option>
        </select>
      </label>
    </>
  );
};

const cloneFilter = (filter: WakeFilterTs): WakeFilterTs =>
  JSON.parse(JSON.stringify(filter)) as WakeFilterTs;

const shortId = (value: string): string => value.slice(0, 8);

const copyText = async (value: string): Promise<void> => {
  await navigator.clipboard?.writeText(value);
};

const wakeSummary = (filter: WakeFilterTs) => {
  switch (filter.kind) {
    case "on_memory":
      return (
        <>
          <span>OnMemory</span>
          <Mono>{filter.schema_id}</Mono>
        </>
      );
    case "on_edge":
      return (
        <>
          <span>OnEdge</span>
          <Mono>{filter.relation_id}</Mono>
        </>
      );
    case "custom":
      return (
        <>
          <span>Custom</span>
          <Mono>{filter.kind_id}</Mono>
        </>
      );
  }
};

const unwrap = async <T, E>(
  result: Promise<{ status: "ok"; data: T } | { status: "error"; error: E }>,
): Promise<T> => {
  const value = await result;
  if (value.status === "error") throw value.error;
  return value.data;
};
