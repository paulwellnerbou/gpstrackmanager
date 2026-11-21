/**
 * The position hovered in the profile or point list, shown on the map. Kept outside React
 * state so that mouse moves do not re-render the app.
 */
type LonLat = [number, number] | null;
type Listener = (p: LonLat) => void;

const listeners = new Set<Listener>();

export const hoverCursor = {
  set(p: LonLat) {
    for (const l of listeners) l(p);
  },
  subscribe(l: Listener) {
    listeners.add(l);
    return () => {
      listeners.delete(l);
    };
  },
};
