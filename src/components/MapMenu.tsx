import { useEffect, useRef, useState } from "react";
import { usePresence } from "../presence";

export type MapTheme = "auto" | "light" | "dark" | "black";
export type TrackColor = "violet" | "blue" | "teal" | "green";

export interface MapOptions {
  theme: MapTheme;
  trackColor: TrackColor;
}

export const DEFAULT_MAP_OPTIONS: MapOptions = { theme: "auto", trackColor: "violet" };

/** Line colors for the unselected tracks, tuned per base-map brightness. */
export const TRACK_COLORS: Record<TrackColor, { label: string; light: string; dark: string }> = {
  violet: { label: "Violet", light: "#6741d9", dark: "#b197fc" },
  blue: { label: "Blue", light: "#1971c2", dark: "#74c0fc" },
  teal: { label: "Teal", light: "#0c8599", dark: "#3bc9db" },
  green: { label: "Green", light: "#2b8a3e", dark: "#8ce99a" },
};

export const MAP_THEMES: { value: MapTheme; label: string; title: string }[] = [
  { value: "auto", label: "Auto", title: "Follow the system light/dark setting" },
  { value: "light", label: "Light", title: "Light map" },
  { value: "dark", label: "Dark", title: "Blue-grey dark map" },
  { value: "black", label: "Black", title: "Near-black dark map" },
];

interface Props {
  options: MapOptions;
  onChange: (o: MapOptions) => void;
}

/** Base map theme and track color. */
export function MapMenu({ options, onChange }: Props) {
  const [open, setOpen] = useState(false);
  const root = useRef<HTMLDivElement>(null);
  const { shown, closing } = usePresence(open);

  useEffect(() => {
    if (!open) return;
    const close = (e: MouseEvent) => !root.current?.contains(e.target as Node) && setOpen(false);
    window.addEventListener("mousedown", close);
    return () => window.removeEventListener("mousedown", close);
  }, [open]);

  return (
    <div className="marker-menu" ref={root}>
      <button className={open ? "open" : undefined} aria-expanded={open} onClick={() => setOpen(!open)}>
        Map <span className="caret">▾</span>
      </button>
      {shown && (
        <div className={closing ? "marker-panel closing" : "marker-panel"}>
          <div className="marker-group">Map theme</div>
          <div className="segmented">
            {MAP_THEMES.map((t) => (
              <button
                key={t.value}
                title={t.title}
                className={options.theme === t.value ? "active" : undefined}
                onClick={() => onChange({ ...options, theme: t.value })}
              >
                {t.label}
              </button>
            ))}
          </div>
          <div className="marker-group">Track color</div>
          <div className="swatches">
            {(Object.keys(TRACK_COLORS) as TrackColor[]).map((c) => (
              <button
                key={c}
                title={TRACK_COLORS[c].label}
                aria-label={TRACK_COLORS[c].label}
                aria-pressed={options.trackColor === c}
                className={options.trackColor === c ? "swatch active" : "swatch"}
                style={{
                  background: `linear-gradient(135deg, ${TRACK_COLORS[c].light} 50%, ${TRACK_COLORS[c].dark} 50%)`,
                }}
                onClick={() => onChange({ ...options, trackColor: c })}
              />
            ))}
          </div>
        </div>
      )}
    </div>
  );
}
