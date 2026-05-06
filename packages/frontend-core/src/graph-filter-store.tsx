import {
  createContext,
  createSignal,
  useContext,
  type Accessor,
  type JSX,
} from "solid-js";

export type GraphLayer = "Fact" | "Abstraction" | "Perspective" | "Goal";
export const CORE_FLAVOR_ID = "__core__";

export interface GraphFilterState {
  layers: ReadonlySet<GraphLayer>;
  schemaIds: ReadonlySet<string>;
  hiddenFlavorIds: ReadonlySet<string>;
  search: string;
}

export interface GraphFilterStore {
  state: Accessor<GraphFilterState>;
  setLayer(kind: GraphLayer, enabled: boolean): void;
  setSchema(schemaId: string, enabled: boolean): void;
  setFlavor(flavorId: string, visible: boolean): void;
  setSearch(value: string): void;
  reset(): void;
}

export const GRAPH_LAYERS: readonly GraphLayer[] = [
  "Fact",
  "Abstraction",
  "Perspective",
  "Goal",
];

export const flavorFilterId = (flavor: string | null): string =>
  flavor ?? CORE_FLAVOR_ID;

export const flavorFilterLabel = (flavorId: string): string =>
  flavorId === CORE_FLAVOR_ID ? "core" : flavorId;

export const defaultGraphFilterState = (): GraphFilterState => ({
  layers: new Set(GRAPH_LAYERS),
  schemaIds: new Set(),
  hiddenFlavorIds: new Set(),
  search: "",
});

const GraphFilterContext = createContext<GraphFilterStore>();

const mutateSet = (
  current: ReadonlySet<string>,
  value: string,
  enabled: boolean,
): ReadonlySet<string> => {
  const next = new Set(current);
  if (enabled) next.add(value);
  else next.delete(value);
  return next;
};

export function createGraphFilterStore(): GraphFilterStore {
  const [state, setState] = createSignal<GraphFilterState>(
    defaultGraphFilterState(),
  );

  return {
    state,
    setLayer(kind, enabled) {
      setState((prev) => ({
        ...prev,
        layers: mutateSet(prev.layers, kind, enabled) as ReadonlySet<GraphLayer>,
      }));
    },
    setSchema(schemaId, enabled) {
      setState((prev) => ({
        ...prev,
        schemaIds: mutateSet(prev.schemaIds, schemaId, enabled),
      }));
    },
    setFlavor(flavorId, visible) {
      setState((prev) => ({
        ...prev,
        hiddenFlavorIds: mutateSet(prev.hiddenFlavorIds, flavorId, !visible),
      }));
    },
    setSearch(value) {
      setState((prev) => ({ ...prev, search: value }));
    },
    reset() {
      setState(defaultGraphFilterState());
    },
  };
}

export const GraphFilterProvider = (props: {
  store: GraphFilterStore;
  children: JSX.Element;
}): JSX.Element => (
  <GraphFilterContext.Provider value={props.store}>
    {props.children}
  </GraphFilterContext.Provider>
);

export const useGraphFilter = (): GraphFilterStore => {
  const store = useContext(GraphFilterContext);
  if (store === undefined) {
    throw new Error("GraphFilterProvider is missing");
  }
  return store;
};
