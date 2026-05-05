import "@proxima/core/styles.css";
import {
  MarketplaceView,
  PlaceholderView,
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
    component: () => <PlaceholderView label="FullSurface" />,
    flavor: null,
  },
  {
    id: "atlas",
    label: "Atlas",
    component: () => <PlaceholderView label="Atlas" />,
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
