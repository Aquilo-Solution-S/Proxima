import "./browser-tauri-fallback";
import "@proxima/core/styles.css";
import { Show, createSignal, lazy, type Component } from "solid-js";
import {
  createHub,
  type Hub,
  type RegisteredSettingsPanel,
  type RegisteredView,
} from "@proxima/core/hub";
import {
  GraphFilterProvider,
  createGraphFilterStore,
} from "@proxima/core";
import { createGraphStore, GraphProvider } from "@proxima/core/graph-store";
import { StartupSimulation } from "@proxima/core/primitives/startup-simulation";
import { Shell } from "@proxima/core/shell";
import { createTauriEngineClient } from "@proxima/core/tauri-client";
import "@proxima/flavor-code/styles.css";
import { initFlavors } from "./flavors";

initFlavors();

const SettingsGeneralPanel = lazy(async () => {
  const { SettingsGeneralPanel } = await import(
    "@proxima/core/views/settings-general"
  );
  return { default: SettingsGeneralPanel };
});

const SettingsModelsPanel = lazy(async () => {
  const { SettingsModelsPanel } = await import(
    "@proxima/core/views/settings-models"
  );
  return { default: SettingsModelsPanel };
});

const SettingsMcpPanel = lazy(async () => {
  const { SettingsMcpPanel } = await import(
    "@proxima/core/views/settings-mcp"
  );
  return { default: SettingsMcpPanel };
});

const viewWithHub = (
  load: () => Promise<{ default: Component }>,
): Component => lazy(load);

const substrateSettingsPanels: RegisteredSettingsPanel[] = [
  {
    id: "general",
    label: "General",
    component: SettingsGeneralPanel,
    flavor: null,
  },
  {
    id: "models",
    label: "Models",
    component: SettingsModelsPanel,
    flavor: null,
  },
  {
    id: "mcp",
    label: "MCP",
    component: SettingsMcpPanel,
    flavor: null,
  },
];

function createAppHub(): Hub {
  let hub!: Hub;
  hub = createHub(
    [
    {
      id: "surface",
      label: "Surface",
      component: viewWithHub(async () => {
        const { FullSurface } = await import("@proxima/core/views/surface");
        return { default: () => <FullSurface hub={hub} /> };
      }),
      flavor: null,
    },
    {
      id: "atlas",
      label: "Atlas",
      component: viewWithHub(async () => {
        const { Atlas } = await import("@proxima/core/views/atlas");
        return { default: () => <Atlas hub={hub} /> };
      }),
      flavor: null,
    },
    {
      id: "personalities",
      label: "Personalities",
      component: viewWithHub(async () => {
        const { PersonalitiesView } = await import(
          "@proxima/core/views/personalities"
        );
        return { default: PersonalitiesView };
      }),
      flavor: null,
    },
    {
      id: "marketplace",
      label: "Marketplace",
      component: viewWithHub(async () => {
        const { MarketplaceView } = await import(
          "@proxima/core/views/marketplace"
        );
        return { default: () => <MarketplaceView hub={hub} /> };
      }),
      flavor: null,
    },
    {
      id: "settings",
      label: "Settings",
      component: viewWithHub(async () => {
        const { SettingsView } = await import("@proxima/core/views/settings");
        return { default: () => <SettingsView hub={hub} /> };
      }),
      flavor: null,
    },
    ] satisfies RegisteredView[],
    [
      ...substrateSettingsPanels,
      {
        id: "schemas",
        label: "Schemas",
        component: viewWithHub(async () => {
          const { SchemasView } = await import("@proxima/core/views/schemas");
          return { default: () => <SchemasView hub={hub} /> };
        }),
        flavor: null,
      },
    ],
  );
  return hub;
}

function App() {
  const hub = createAppHub();
  const graph = createGraphStore(createTauriEngineClient(), hub);
  const filters = createGraphFilterStore();
  const [startupComplete, setStartupComplete] = createSignal(false);

  return (
    <>
      <GraphProvider store={graph}>
        <GraphFilterProvider store={filters}>
          <Shell hub={hub} />
        </GraphFilterProvider>
      </GraphProvider>
      <Show when={!startupComplete()}>
        <StartupSimulation onComplete={() => setStartupComplete(true)} />
      </Show>
    </>
  );
}

export default App;
