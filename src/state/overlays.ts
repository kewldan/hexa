// Highlight layers contributed by panels (structure fields, diff regions…),
// keyed by tab and layer name (DISP-16).
import { create } from "zustand";

export interface OverlayRegion {
  start: number;
  end: number;
  color: string;
  /** Shown in the hover tooltip. */
  label?: string;
}

interface OverlayStore {
  layers: Record<number, Record<string, OverlayRegion[]>>;
  set: (tab: number, layer: string, regions: OverlayRegion[]) => void;
  clear: (tab: number, layer: string) => void;
}

export const useOverlays = create<OverlayStore>((set) => ({
  layers: {},
  set: (tab, layer, regions) =>
    set((st) => ({
      layers: { ...st.layers, [tab]: { ...(st.layers[tab] ?? {}), [layer]: regions } },
    })),
  clear: (tab, layer) =>
    set((st) => {
      const cur = { ...(st.layers[tab] ?? {}) };
      delete cur[layer];
      return { layers: { ...st.layers, [tab]: cur } };
    }),
}));
