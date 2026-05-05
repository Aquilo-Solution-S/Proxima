import "@proxima/core/styles.css";
import {
  PlaceholderView,
  SettingsView,
  Shell,
  createHub,
  type RegisteredView,
} from "@proxima/core";

const SUBSTRATE_VIEWS: RegisteredView[] = [
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
    component: () => <PlaceholderView label="Schemas" />,
    flavor: null,
  },
  {
    id: "marketplace",
    label: "Marketplace",
    component: () => <PlaceholderView label="Marketplace" />,
    flavor: null,
  },
  {
    id: "settings",
    label: "Settings",
    component: SettingsView,
    flavor: null,
  },
];

const hub = createHub(SUBSTRATE_VIEWS);
// Future: registerCode(hub.registerFlavor.bind(hub));

function App() {
  return <Shell hub={hub} />;
}

export default App;
