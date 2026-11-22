import { useEffect, useMemo, useState } from "react";
import { api, formatDuration, type CleanupPlan, type PointTable, type Stay } from "../api";
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

const DEFAULTS = { radiusM: 50, minMinutes: 10, stays: true, zeros: true, spikes: true };

function loadSettings(): typeof DEFAULTS {
  try {
    const raw = localStorage.getItem(SETTINGS_KEY);
    if (raw) return { ...DEFAULTS, ...JSON.parse(raw) };
  } catch {
    // Defaults below.
  }
  return DEFAULTS;
}

const NOTHING: CleanupPlan = { stays: [], spikes: [] };

/** Every point the clean-up touches, shown as the selection. Stays never overlap. */
function touched(plan: CleanupPlan, zeros: number[]) {
  const set = new Set<number>([...zeros, ...plan.spikes]);
  for (const s of plan.stays) for (let i = s.start; i <= s.end; i++) set.add(i);
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
 * and collapses each to its spot, and/or removes fixes at a 0 coordinate and spikes far
 * off the track. What would be changed is shown as the point selection until applied.
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
  // The plan and the settings it was found with; only a shown preview can be applied.
  const [preview, setPreview] = useState<{
    radiusM: number | null;
    minDurationS: number;
    zeros: boolean;
    spikes: boolean;
    plan: CleanupPlan;
  } | null>(null);
  const { radiusM, minMinutes } = settings;
  const staysValid = radiusM > 0 && minMinutes > 0;
  // Invalid stay settings leave the other options to clean up on their own.
  const findStays = settings.stays && staysValid;
  const askBackend = findStays || settings.spikes;
  // Null while the backend is looking.
  const plan = !askBackend
    ? NOTHING
    : preview?.zeros === settings.zeros &&
        preview.spikes === settings.spikes &&
        (preview.radiusM != null) === findStays
      ? preview.plan
      : null;
  const zeros = useMemo(() => (table && settings.zeros ? zeroIndices(table) : []), [table, settings.zeros]);
  const selected = useMemo(() => (plan ? touched(plan, zeros) : null), [plan, zeros]);
  // Zero fixes and spikes are never a stay's arrival or departure: stays are found without them.
  const removed = selected && plan ? selected.size - 2 * plan.stays.length : 0;

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
    if (!table || !askBackend) return;
    let cancelled = false;
    const radius = findStays ? radiusM : null;
    const minDurationS = minMinutes * 60;
    const { zeros, spikes } = settings;
    const timer = setTimeout(() => {
      api
        .previewCleanUp(trackId, radius, minDurationS, zeros, spikes)
        .then((plan) => !cancelled && setPreview({ radiusM: radius, minDurationS, zeros, spikes, plan }))
        .catch((e) => !cancelled && onError(String(e)));
    }, 250);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [trackId, table, radiusM, minMinutes, findStays, askBackend, settings.zeros, settings.spikes, onError]);

  useEffect(() => {
    if (!table || !plan || !selected) return;
    const first = Math.min(plan.stays[0]?.start ?? Infinity, zeros[0] ?? Infinity, plan.spikes[0] ?? Infinity);
    onSelectionChange({ set: selected, anchor: Number.isFinite(first) ? first : null });
  }, [table, plan, zeros, selected, onSelectionChange]);

  const apply = async () => {
    if (!plan) return;
    p.onBeginEdit();
    try {
      await api.cleanUpTrack(
        trackId,
        findStays ? (preview?.radiusM ?? null) : null,
        preview?.minDurationS ?? 0,
        settings.zeros,
        settings.spikes,
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
      <label className="cleanup-option">
        <input
          type="checkbox"
          checked={settings.spikes}
          onChange={(e) => setSettings({ ...settings, spikes: e.target.checked })}
        />
        Remove spikes far off the track
      </label>
      <p className="muted">
        Up to 10 points more than 100 km away that the track jumps to and back, as left by a wrongly decoded log.
        Kept when their times make the trip possible.
      </p>
      <div className="cleanup-result">
        {!settings.stays && !settings.zeros && !settings.spikes
          ? "Choose what to clean up."
          : plan == null
            ? findStays
              ? "Looking for stays…"
              : "Looking for spikes…"
            : [
                settings.stays && !staysValid && "Enter a radius and a duration to find stays.",
                removed === 0
                  ? "Nothing to clean up."
                  : [
                      plan.stays.length > 0 &&
                        `${plan.stays.length} ${plan.stays.length === 1 ? "stay" : "stays"}`,
                      zeros.length > 0 && `${pointsLabel(zeros.length)} at 0`,
                      plan.spikes.length > 0 &&
                        `${plan.spikes.length} ${plan.spikes.length === 1 ? "spike" : "spikes"}`,
                      `${pointsLabel(removed)} to remove`,
                    ]
                      .filter(Boolean)
                      .join(" · "),
              ]
                .filter(Boolean)
                .join(" ")}
      </div>
      {plan && plan.stays.length > 0 && table && (
        <ul className="cleanup-stays">
          {plan.stays.map((s) => (
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
