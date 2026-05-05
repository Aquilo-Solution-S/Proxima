import "@proxima/core/styles.css";
import { Show, createSignal, lazy, type Component } from "solid-js";
import {
  createHub,
  type FlavorScope,
  type RegisteredSettingsPanel,
  type RegisteredView,
} from "@proxima/core/hub";
import { StartupSimulation } from "@proxima/core/primitives/startup-simulation";
import { Shell } from "@proxima/core/shell";

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

function registerCodeFlavor(scope: FlavorScope): void {
  scope.registerView({
    id: "code",
    label: "Code",
    component: lazy(async () => {
      const { CodeView } = await import("@proxima/core/flavors/code");
      return { default: CodeView };
    }),
  });
}

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
];

const hub = createHub(
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
      id: "schemas",
      label: "Schemas",
      component: viewWithHub(async () => {
        const { SchemasView } = await import("@proxima/core/views/schemas");
        return { default: () => <SchemasView hub={hub} /> };
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
  substrateSettingsPanels,
);
hub.registerFlavor("code", registerCodeFlavor);

function App() {
  const [startupComplete, setStartupComplete] = createSignal(false);

  return (
    <>
      <Shell hub={hub} />
      <Show when={!startupComplete()}>
        <StartupSimulation onComplete={() => setStartupComplete(true)} />
      </Show>
    </>
  );
}

export default App;
