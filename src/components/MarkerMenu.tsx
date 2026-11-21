import { useEffect, useRef, useState } from "react";
import type { MarkerOptions } from "./trackMarkers";

interface Props {
  options: MarkerOptions;
  onChange: (o: MarkerOptions) => void;
}

const ITEMS: { key: keyof MarkerOptions; label: string; group: "show" | "label" }[] = [
  { key: "startEnd", label: "Start / End", group: "show" },
  { key: "distance", label: "Distance markers", group: "show" },
  { key: "waypoints", label: "Waypoints and laps", group: "show" },
  { key: "showDistance", label: "Distance", group: "label" },
  { key: "showSpeed", label: "Speed", group: "label" },
  { key: "showAltitude", label: "Altitude", group: "label" },
  { key: "showTime", label: "Time", group: "label" },
];

/** Marker settings for the selected track and selected points. */
export function MarkerMenu({ options, onChange }: Props) {
  const [open, setOpen] = useState(false);
  const root = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const close = (e: MouseEvent) => !root.current?.contains(e.target as Node) && setOpen(false);
    window.addEventListener("mousedown", close);
    return () => window.removeEventListener("mousedown", close);
  }, [open]);

  const row = (item: (typeof ITEMS)[number]) => (
    <label key={item.key}>
      <input
        type="checkbox"
        checked={options[item.key]}
        onChange={(e) => onChange({ ...options, [item.key]: e.target.checked })}
      />
      {item.label}
    </label>
  );

  return (
    <div className="marker-menu" ref={root}>
      <button onClick={() => setOpen(!open)}>Markers ▾</button>
      {open && (
        <div className="marker-panel">
          <div className="marker-group">Show for selected track</div>
          {ITEMS.filter((i) => i.group === "show").map(row)}
          <div className="marker-group">Labels (distance markers and selected points)</div>
          {ITEMS.filter((i) => i.group === "label").map(row)}
        </div>
      )}
    </div>
  );
}
