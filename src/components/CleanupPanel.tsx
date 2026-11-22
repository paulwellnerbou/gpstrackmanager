import { useEffect, useState } from "react";
import { api, formatDuration, type PointTable, type Stay } from "../api";
import { bboxOf, pointsLabel, type PointSelection } from "../pointEditing";
import { spansDays } from "../timeFormat";

interface Props {
  trackId: number;
  table: PointTable | null;
  locked: boolean;
  onSelectionChange: (s: PointSelection) => void;
  onZoomTo: (bbox: [number, number, number, number]) => void;
  onBeginEdit: () => void;
  onEdited: () => void;
  onError: (message: string) => void;
  onClose: () => void;
}

const SETTINGS_KEY = "cleanupSettings";

function loadSettings() {
  try {
    const raw = localStorage.getItem(SETTINGS_KEY);
    if (raw) return { radiusM: 50, minMinutes: 10, ...JSON.parse(raw) };
  } catch {
    // Defaults below.
  }
  return { radiusM: 50, minMinutes: 10 };
}

/** Points removed when a stay collapses to its first and last fix. */
const removedBy = (s: Stay) => Math.max(0, s.end - s.start - 1);

function stayIndices(stays: Stay[]) {
  const set = new Set<number>();
  for (const s of stays) for (let i = s.start; i <= s.end; i++) set.add(i);
  return set;
}

const known = (ms: number) => (Number.isNaN(ms) ? null : ms);


/**
 * Finds stays (the logger lay still and its fixes scattered in a star around one spot)
 * and collapses each to its spot. The stays are shown as the point selection until applied.
 */
export function CleanupPanel(p: Props) {
  const { trackId, table, onSelectionChange, onError } = p;
  const multiDay = table != null && spansDays(table.time);
  const clock = (ms: number) =>
    Number.isNaN(ms)
      ? "—"
      : new Date(ms).toLocaleString(undefined, {
          ...(multiDay ? { day: "2-digit", month: "2-digit" } : {}),
          hour: "2-digit",
          minute: "2-digit",
        });
  const [settings, setSettings] = useState(loadSettings);
  // The stays and the settings they were found with; only a shown preview can be applied.
  const [preview, setPreview] = useState<{ radiusM: number; minDurationS: number; stays: Stay[] } | null>(null);
  const stays = preview?.stays ?? null;
  const { radiusM, minMinutes } = settings;
  const valid = radiusM > 0 && minMinutes > 0;

  useEffect(() => {
    try {
      localStorage.setItem(SETTINGS_KEY, JSON.stringify(settings));
    } catch {
      // Only a convenience.
    }
  }, [settings]);

  // Waits for the table so the preview selection refers to the indices shown.
  useEffect(() => {
    setPreview(null);
    if (!table || !valid) return;
    let cancelled = false;
    const minDurationS = minMinutes * 60;
    const timer = setTimeout(() => {
      api
        .findTrackStays(trackId, radiusM, minDurationS)
        .then((s) => {
          if (cancelled) return;
          setPreview({ radiusM, minDurationS, stays: s });
          onSelectionChange({ set: stayIndices(s), anchor: s[0]?.start ?? null });
        })
        .catch((e) => !cancelled && onError(String(e)));
    }, 250);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [trackId, table, radiusM, minMinutes, valid, onSelectionChange, onError]);

  const removed = stays?.reduce((n, s) => n + removedBy(s), 0) ?? 0;

  const apply = async () => {
    if (!preview) return;
    p.onBeginEdit();
    try {
      await api.cleanUpTrack(trackId, preview.radiusM, preview.minDurationS);
      p.onClose();
      p.onEdited();
    } catch (e) {
      onError(String(e));
    }
  };

  // Every stay stays selected: all of them are cleaned up, not just the one in view.
  const zoomTo = (s: Stay) => {
    if (!table || !stays) return;
    onSelectionChange({ set: stayIndices(stays), anchor: s.start });
    const b = bboxOf(table, Array.from({ length: s.end - s.start + 1 }, (_, k) => s.start + k));
    if (b) p.onZoomTo(b);
  };

  const number = (key: "radiusM" | "minMinutes", label: string, unit: string) => (
    <label>
      {label}
      <span className="with-unit">
        <input
          type="number"
          min={1}
          value={settings[key]}
          onChange={(e) => setSettings({ ...settings, [key]: e.target.valueAsNumber || 0 })}
        />
        <span className="muted">{unit}</span>
      </span>
    </label>
  );

  return (
    <div className="cleanup">
      <div className="cleanup-title">Clean up track</div>
      <p className="muted">
        Collapses stays, where the logger lay still (e.g. left on indoors) and its positions jump around one spot, to a
        single position. Arrival and departure times are kept.
      </p>
      <div className="cleanup-fields">
        {number("radiusM", "Scatter radius", "m")}
        {number("minMinutes", "At least", "min")}
      </div>
      <div className="cleanup-result">
        {!valid
          ? "Enter a radius and a duration."
          : stays == null
            ? "Looking for stays…"
            : stays.length === 0
              ? "No stays found."
              : `${stays.length} ${stays.length === 1 ? "stay" : "stays"} · ${pointsLabel(removed)} to remove`}
      </div>
      {stays && stays.length > 0 && table && (
        <ul className="cleanup-stays">
          {stays.map((s) => (
            <li key={s.start}>
              <button className="link" onClick={() => zoomTo(s)} title="Zoom to this stay">
                {clock(table.time[s.start])}–{clock(table.time[s.end])}
              </button>
              <span className="muted">
                {formatDuration(known(table.time[s.start]), known(table.time[s.end]))} · {pointsLabel(s.end - s.start + 1)}
              </span>
            </li>
          ))}
        </ul>
      )}
      <div className="cleanup-actions">
        <button onClick={p.onClose}>Cancel</button>
        <button className="primary" disabled={p.locked || !stays?.length} onClick={apply}>
          Clean up
        </button>
      </div>
    </div>
  );
}
