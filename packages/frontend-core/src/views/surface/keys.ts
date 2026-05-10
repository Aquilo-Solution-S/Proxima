import type { ActiveTab } from "./row-list";

export interface KeyHandlers {
  onTab: (tab: ActiveTab) => void;
  onToggleFilters: () => void;
  onToggleEventStream: () => void;
  onCloseDrawer: () => void;
}

export const installSurfaceKeys = (handlers: KeyHandlers): (() => void) => {
  const onKey = (e: KeyboardEvent) => {
    const meta = e.metaKey || e.ctrlKey;
    if (!meta && e.key !== "Escape") return;
    if (meta && e.key === "1") { handlers.onTab("All"); e.preventDefault(); }
    else if (meta && e.key === "2") { handlers.onTab("Perspective"); e.preventDefault(); }
    else if (meta && e.key === "3") { handlers.onTab("Abstraction"); e.preventDefault(); }
    else if (meta && e.key === "4") { handlers.onTab("Fact"); e.preventDefault(); }
    else if (meta && e.key === "5") { handlers.onTab("Goal"); e.preventDefault(); }
    else if (meta && e.key.toLowerCase() === "f") { handlers.onToggleFilters(); e.preventDefault(); }
    else if (meta && e.key.toLowerCase() === "e") { handlers.onToggleEventStream(); e.preventDefault(); }
    else if (e.key === "Escape") { handlers.onCloseDrawer(); }
  };
  window.addEventListener("keydown", onKey);
  return () => window.removeEventListener("keydown", onKey);
};
