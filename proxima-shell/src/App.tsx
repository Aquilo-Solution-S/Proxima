import "@proxima/core/styles.css";
import {
  Atlas,
  FullSurface,
  MarketplaceView,
  SchemasView,
  SettingsView,
  Shell,
  createHub,
  type RegisteredView,
} from "@proxima/core";

const hub = createHub([
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
    component: SettingsView,
    flavor: null,
  },
] satisfies RegisteredView[]);
// Future: registerCode(hub.registerFlavor.bind(hub));

function App() {
  return <Shell hub={hub} />;
}

export default App;
