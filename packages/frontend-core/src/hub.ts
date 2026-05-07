import {
  createEffect,
  createSignal,
  type Accessor,
  type Component,
  type JSX,
  type Setter,
} from "solid-js";
import type { MemoryRow } from "./bindings";
import {
  getPayloadRenderer,
  registeredFlavorNames,
  registeredPayloadRenderers,
  registeredShellViews,
} from "./registry";

export interface Renderer<T = unknown> {
  render: (props: { memory: MemoryRow; payload: T }) => JSX.Element;
}

export type NaturalKey = ReadonlyArray<string | number>;

export interface PayloadCodec<T = unknown> {
  decode: (bytes: Uint8Array) => T;
  encode: (value: T) => Uint8Array;
  naturalKey?: (value: T) => NaturalKey | null;
}

export interface RegisteredView {
  id: string;
  label: string;
  component: Component;
  /** `null` for substrate-owned views; flavor name for flavor-registered. */
  flavor: string | null;
}

export interface RegisteredSettingsPanel {
  id: string;
  label: string;
  component: Component;
  /** `null` for substrate-owned; flavor name for flavor-registered. */
  flavor: string | null;
}

export interface RegisteredRenderer {
  schemaId: string;
  schemaVersion: number;
  flavor: string;
}

export interface RegisteredCodec {
  schemaId: string;
  schemaVersion: number;
  flavor: string;
}

export interface FlavorScope {
  registerCodec<T>(
    schemaId: string,
    schemaVersion: number,
    codec: PayloadCodec<T>,
  ): void;
  registerRenderer<T>(
    schemaId: string,
    schemaVersion: number,
    renderer: Renderer<T>,
  ): void;
  registerView(view: {
    id: string;
    label: string;
    component: Component;
  }): void;
  registerSettingsPanel(panel: {
    id: string;
    label: string;
    component: Component;
  }): void;
}

export interface Hub {
  registerFlavor(
    name: string,
    register: (scope: FlavorScope) => void,
  ): void;
  rendererFor(
    schemaId: string,
    schemaVersion: number,
    kind?: MemoryRow["kind"],
  ): Renderer<unknown> | null;
  codecFor(
    schemaId: string,
    schemaVersion: number,
  ): PayloadCodec<unknown> | null;
  flavorFor(schemaId: string, schemaVersion: number): string | null;
  views: Accessor<RegisteredView[]>;
  currentView: Accessor<string>;
  setCurrentView: Setter<string>;
  registeredFlavors: Accessor<string[]>;
  registeredRenderers: Accessor<RegisteredRenderer[]>;
  registeredCodecs: Accessor<RegisteredCodec[]>;
  settingsPanels: Accessor<RegisteredSettingsPanel[]>;
  currentSettingsPanel: Accessor<string>;
  setCurrentSettingsPanel: Setter<string>;
}

const rendererKey = (id: string, v: number): string => `${id}@${v}`;

export function createHub(
  substrateViews: RegisteredView[],
  substrateSettingsPanels: RegisteredSettingsPanel[] = [],
): Hub {
  const renderers = new Map<string, Renderer<unknown>>();
  const codecs = new Map<string, PayloadCodec<unknown>>();
  for (const registration of registeredPayloadRenderers()) {
    renderers.set(
      rendererKey(registration.schemaId, registration.schemaVersion),
      registration.renderer,
    );
    if (registration.codec !== null) {
      codecs.set(
        rendererKey(registration.schemaId, registration.schemaVersion),
        registration.codec,
      );
    }
  }
  const [views, setViews] = createSignal<RegisteredView[]>([
    ...substrateViews,
    ...registeredShellViews(),
  ]);
  const [flavors, setFlavors] = createSignal<string[]>(registeredFlavorNames());
  const [renderersList, setRenderersList] = createSignal<RegisteredRenderer[]>(
    registeredPayloadRenderers().map((registration) => ({
      schemaId: registration.schemaId,
      schemaVersion: registration.schemaVersion,
      flavor: registration.flavor,
    })),
  );
  const [codecsList, setCodecsList] = createSignal<RegisteredCodec[]>(
    registeredPayloadRenderers()
      .filter((registration) => registration.codec !== null)
      .map((registration) => ({
        schemaId: registration.schemaId,
        schemaVersion: registration.schemaVersion,
        flavor: registration.flavor,
      })),
  );
  const [currentView, setCurrentView] = createSignal<string>(
    substrateViews[0]?.id ?? "",
  );
  const [settingsPanels, setSettingsPanels] = createSignal<
    RegisteredSettingsPanel[]
  >(substrateSettingsPanels);
  const [currentSettingsPanel, setCurrentSettingsPanel] = createSignal<string>(
    substrateSettingsPanels[0]?.id ?? "",
  );

  createEffect(() => {
    const panels = settingsPanels();
    const current = currentSettingsPanel();
    if (panels.find((p) => p.id === current) === undefined && panels.length > 0) {
      setCurrentSettingsPanel(panels[0].id);
    }
  });

  return {
    registerFlavor(name, register) {
      setFlavors((prev) => (prev.includes(name) ? prev : [...prev, name]));
      register({
        registerCodec: (sid, sver, c) => {
          codecs.set(rendererKey(sid, sver), c as PayloadCodec<unknown>);
          setCodecsList((prev) => [
            ...prev.filter(
              (entry) =>
                entry.schemaId !== sid || entry.schemaVersion !== sver,
            ),
            { schemaId: sid, schemaVersion: sver, flavor: name },
          ]);
        },
        registerRenderer: (sid, sver, r) => {
          renderers.set(rendererKey(sid, sver), r as Renderer<unknown>);
          setRenderersList((prev) => [
            ...prev.filter(
              (entry) =>
                entry.schemaId !== sid || entry.schemaVersion !== sver,
            ),
            { schemaId: sid, schemaVersion: sver, flavor: name },
          ]);
        },
        registerView: (view) => {
          setViews((prev) => [...prev, { ...view, flavor: name }]);
        },
        registerSettingsPanel: (panel) => {
          setSettingsPanels((prev) => [...prev, { ...panel, flavor: name }]);
        },
      });
    },
    rendererFor: (sid, sver, kind) =>
      getPayloadRenderer(sid, sver, kind)?.renderer ??
      renderers.get(rendererKey(sid, sver)) ??
      null,
    codecFor: (sid, sver) => codecs.get(rendererKey(sid, sver)) ?? null,
    flavorFor: (sid, sver) => {
      const codec = codecsList().find(
        (c) => c.schemaId === sid && c.schemaVersion === sver,
      );
      if (codec !== undefined) return codec.flavor;
      return renderersList().find(
        (r) => r.schemaId === sid && r.schemaVersion === sver,
      )?.flavor ?? null;
    },
    views,
    currentView,
    setCurrentView,
    registeredFlavors: flavors,
    registeredRenderers: renderersList,
    registeredCodecs: codecsList,
    settingsPanels,
    currentSettingsPanel,
    setCurrentSettingsPanel,
  };
}
