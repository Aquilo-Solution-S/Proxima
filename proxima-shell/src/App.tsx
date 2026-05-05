import "@proxima/core/styles.css";
import {
  Atlas,
  FullSurface,
  MarketplaceView,
  SchemasView,
  SettingsView,
  SettingsGeneralPanel,
  Shell,
  createHub,
  type RegisteredSettingsPanel,
  type RegisteredView,
} from "@proxima/core";

const substrateSettingsPanels: RegisteredSettingsPanel[] = [
  {
    id: "general",
    label: "General",
    component: SettingsGeneralPanel,
    flavor: null,
  },
];

const hub = createHub(
  [
    {
      id: "surface",
      label: "Surface",
      component: () => <FullSurface hub={hub} />,
      flavor: null,
    },
    {
      id: "atlas",
      label: "Atlas",
      component: () => <Atlas hub={hub} />,
      flavor: null,
    },
    {
      id: "schemas",
      label: "Schemas",
      component: () => <SchemasView hub={hub} />,
      flavor: null,
    },
    {
      id: "marketplace",
      label: "Marketplace",
      component: () => <MarketplaceView hub={hub} />,
      flavor: null,
    },
    {
      id: "settings",
      label: "Settings",
      component: () => <SettingsView hub={hub} />,
      flavor: null,
    },
  ] satisfies RegisteredView[],
  substrateSettingsPanels,
);
// Future: registerCode(hub.registerFlavor.bind(hub));

function App() {
  return <Shell hub={hub} />;
}

export default App;
