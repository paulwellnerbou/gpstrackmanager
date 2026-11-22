import { useEffect, useMemo, useState } from "react";
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

const DEFAULTS = { radiusM: 50, minMinutes: 10, stays: true, zeros: true };

function loadSettings(): typeof DEFAULTS {
  try {
    const raw = localStorage.getItem(SETTINGS_KEY);
    if (raw) return { ...DEFAULTS, ...JSON.parse(raw) };
  } catch {
    // Defaults below.
  }
  return DEFAULTS;
}

const NO_STAYS: Stay[] = [];

/** Every point the clean-up touches, shown as the selection. Stays never overlap. */
function touched(stays: Stay[], zeros: number[]) {
  const set = new Set<number>(zeros);
  for (const s of stays) for (let i = s.start; i <= s.end; i++) set.add(i);
  return set;
}

/** Matches the backend's `is_zero_fix`: loggers write 0 for a coordinate they have no fix for. */
function zeroIndices(t: PointTable) {
  const out: number[] = [];
  for (let i = 0; i < t.length; i++) if (t.lat[i] === 0 || t.lon[i] === 0) out.push(i);
  return out;
}


const known = (ms: number) => (Number.isNaN(ms) ? null : ms);


/**
 * Finds stays (the logger lay still and its fixes scattered in a star around one spot)
 * and collapses each to its spot, and/or removes fixes at a 0 coordinate. What would be
 * changed is shown as the point selection until applied.
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
  const [preview, setPreview] = useState<{
    radiusM: number;
    minDurationS: number;
    zeros: boolean;
    stays: Stay[];
  } | null>(null);
  const { radiusM, minMinutes } = settings;
  const staysValid = radiusM > 0 && minMinutes > 0;
  // Invalid stay settings leave the zero fixes to clean up on their own.
  const findStays = settings.stays && staysValid;
  // Null while stays are being looked for.
  const stays = !findStays ? NO_STAYS : preview?.zeros === settings.zeros ? preview.stays : null;
  const zeros = useMemo(() => (table && settings.zeros ? zeroIndices(table) : []), [table, settings.zeros]);
  const selected = useMemo(() => (stays ? touched(stays, zeros) : null), [stays, zeros]);
  // Zero fixes are never a stay's arrival or departure: stays are found without them.
  const removed = selected && stays ? selected.size - 2 * stays.length : 0;

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
    if (!table || !findStays) return;
    let cancelled = false;
    const minDurationS = minMinutes * 60;
    const zeros = settings.zeros;
    const timer = setTimeout(() => {
      api
        .findTrackStays(trackId, radiusM, minDurationS, zeros)
        .then((s) => !cancelled && setPreview({ radiusM, minDurationS, zeros, stays: s }))
        .catch((e) => !cancelled && onError(String(e)));
    }, 250);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [trackId, table, radiusM, minMinutes, findStays, settings.zeros, onError]);

  useEffect(() => {
    if (!table || !stays || !selected) return;
    const first = Math.min(stays[0]?.start ?? Infinity, zeros[0] ?? Infinity);
    onSelectionChange({ set: selected, anchor: Number.isFinite(first) ? first : null });
  }, [table, stays, zeros, selected, onSelectionChange]);

  const apply = async () => {
    if (!stays) return;
    p.onBeginEdit();
    try {
      await api.cleanUpTrack(
        trackId,
        findStays && preview ? preview.radiusM : null,
        preview?.minDurationS ?? 0,
        settings.zeros,
      );
      p.onClose();
      p.onEdited();
    } catch (e) {
      onError(String(e));
    }
  };

  // Everything stays selected: all of it is cleaned up, not just the stay in view.
  const zoomTo = (s: Stay) => {
    if (!table || !selected) return;
    onSelectionChange({ set: selected, anchor: s.start });
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
      <label className="cleanup-option">
        <input
          type="checkbox"
          checked={settings.stays}
          onChange={(e) => setSettings({ ...settings, stays: e.target.checked })}
        />
        Collapse stays
      </label>
      <p className="muted">
        Where the logger lay still (e.g. left on indoors) and its positions jump around one spot, only that spot is
        kept, with the arrival and departure times.
      </p>
      {settings.stays && (
        <div className="cleanup-fields">
          {number("radiusM", "Scatter radius", "m")}
          {number("minMinutes", "At least", "min")}
        </div>
      )}
      <label className="cleanup-option">
        <input
          type="checkbox"
          checked={settings.zeros}
          onChange={(e) => setSettings({ ...settings, zeros: e.target.checked })}
        />
        Remove points at latitude or longitude 0
      </label>
      <p className="muted">Loggers without a fix often write 0 instead of a position.</p>
      <div className="cleanup-result">
        {!settings.stays && !settings.zeros
          ? "Choose what to clean up."
          : stays == null
            ? "Looking for stays…"
            : [
                settings.stays && !staysValid && "Enter a radius and a duration to find stays.",
                removed === 0
                  ? "Nothing to clean up."
                  : [
                      stays.length > 0 && `${stays.length} ${stays.length === 1 ? "stay" : "stays"}`,
                      zeros.length > 0 && `${pointsLabel(zeros.length)} at 0`,
                      `${pointsLabel(removed)} to remove`,
                    ]
                      .filter(Boolean)
                      .join(" · "),
              ]
                .filter(Boolean)
                .join(" ")}
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
        <button className="primary" disabled={p.locked || removed === 0} onClick={apply}>
          Clean up
        </button>
      </div>
    </div>
  );
}
