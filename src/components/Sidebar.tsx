import { useEffect, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import {
  activityOf,
  api,
  displayName,
  formatDistance,
  type FileProblem,
  type GpsDevice,
  type Root,
  type ScanProgress,
  type TrackSummary,
} from "../api";

interface Props {
  roots: Root[];
  tracks: TrackSummary[];
  selectedId: number | null;
  scan: ScanProgress | null;
  problems: FileProblem[];
  query: string;
  onQuery: (q: string) => void;
  onSelect: (id: number) => void;
  onRootsChanged: () => void;
  onScan: (rootId: number) => void;
  onImported: () => void;
}

export function Sidebar(p: Props) {
  const [devices, setDevices] = useState<GpsDevice[] | null>(null);
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const [deviceMsg, setDeviceMsg] = useState<string | null>(null);
  const [showProblems, setShowProblems] = useState(false);

  // Selection can come from the map, so bring the list entry into view.
  useEffect(() => {
    if (p.selectedId == null) return;
    document.querySelector(`[data-track-id="${p.selectedId}"]`)?.scrollIntoView({ block: "nearest" });
  }, [p.selectedId]);

  const addFolder = async () => {
    const dir = await open({ directory: true, title: "Add a folder to the library" });
    if (typeof dir !== "string") return;
    const root = await api.addRoot(dir);
    p.onRootsChanged();
    p.onScan(root.id);
  };

  const checkDevices = async () => {
    setDeviceMsg(null);
    const found = await api.listGpsDevices();
    setDevices(found);
    setPicked(new Set(found.flatMap((d) => d.files.filter((f) => !f.imported).map((f) => f.path))));
    if (!found.length) setDeviceMsg("No GPS device with track files found. Is it connected and visible in Finder?");
  };

  const importPicked = async () => {
    if (!p.roots.length) return setDeviceMsg("Add a library folder first.");
    const res = await api.importDeviceFiles(p.roots[0].id, [...picked]);
    setDeviceMsg(
      `Imported ${res.copied.length} file(s) into ${p.roots[0].path}` +
        (res.skipped ? `, ${res.skipped} already known` : ""),
    );
    await checkDevices();
    p.onImported();
  };

  const busy = p.scan && p.scan.phase !== "done";
  const unsupported = p.problems.filter((f) => f.status === "unsupported").length;
  const errors = p.problems.filter((f) => f.status === "error").length;
  const missing = p.problems.filter((f) => f.missing).length;

  return (
    <nav className="sidebar">
      <section>
        <header>
          <h2>Library</h2>
          <button onClick={addFolder}>Add folder…</button>
        </header>
        {p.roots.map((r) => (
          <div key={r.id} className="root">
            <span title={r.path}>{r.path}</span>
            <button disabled={!!busy} onClick={() => p.onScan(r.id)} title="Look for new and changed files">
              Sync
            </button>
          </div>
        ))}
        {busy && (
          <div className="progress">
            {p.scan!.phase === "listing" ? "Looking for files…" : `Importing ${p.scan!.done} / ${p.scan!.total}`}
            <progress max={p.scan!.total || 1} value={p.scan!.done} />
          </div>
        )}
        {p.scan?.phase === "done" && (
          <div className="muted">
            Last sync: {p.scan.done} file(s) imported in {(p.scan.elapsedMs / 1000).toFixed(1)} s
          </div>
        )}
        {(unsupported > 0 || errors > 0 || missing > 0) && (
          <button className="link" onClick={() => setShowProblems(!showProblems)}>
            {[
              errors && `${errors} errors`,
              missing && `${missing} missing`,
              unsupported && `${unsupported} unsupported`,
            ]
              .filter(Boolean)
              .join(" · ")}
          </button>
        )}
        {showProblems && (
          <ul className="problems">
            {p.problems.map((f) => (
              <li key={f.path} title={f.error ?? undefined}>
                <b>{f.missing ? "missing" : f.status}</b> {f.path.split("/").slice(-2).join("/")}
                {f.error && <div className="muted">{f.error}</div>}
              </li>
            ))}
          </ul>
        )}
      </section>

      <section>
        <header>
          <h2>Device</h2>
          <button onClick={checkDevices}>Check for GPS device</button>
        </header>
        {devices?.map((d) => (
          <div key={d.volume} className="device">
            <div>
              <b>{d.model ?? d.volume.split("/").pop()}</b> <span className="muted">{d.volume}</span>
            </div>
            {d.files.length === 0 && <div className="muted">No track files on the device.</div>}
            <ul>
              {d.files.map((f) => (
                <li key={f.path}>
                  <label className={f.imported ? "muted" : undefined}>
                    <input
                      type="checkbox"
                      disabled={f.imported}
                      checked={picked.has(f.path)}
                      onChange={(e) => {
                        const next = new Set(picked);
                        if (e.target.checked) next.add(f.path);
                        else next.delete(f.path);
                        setPicked(next);
                      }}
                    />
                    {f.name} {f.imported && "(imported)"}
                  </label>
                </li>
              ))}
            </ul>
          </div>
        ))}
        {devices && picked.size > 0 && <button onClick={importPicked}>Import {picked.size} file(s)</button>}
        {deviceMsg && <div className="muted">{deviceMsg}</div>}
      </section>

      <section className="list-section">
        <input
          className="search"
          type="search"
          placeholder={`Search ${p.tracks.length} tracks…`}
          value={p.query}
          onChange={(e) => p.onQuery(e.target.value)}
        />
        <ul className="track-list">
          {p.tracks.map((t) => (
            <li
              key={t.id}
              data-track-id={t.id}
              className={t.id === p.selectedId ? "selected" : undefined}
              onClick={() => p.onSelect(t.id)}
            >
              <div className="track-name">
                {activityOf(t) && (
                  <span className="activity-icon" title={activityOf(t)!.label}>
                    {activityOf(t)!.icon}
                  </span>
                )}
                {displayName(t)}
              </div>
              <div className="track-meta">
                {t.startTime ? new Date(t.startTime).toLocaleDateString() : "no date"} · {formatDistance(t.distanceM)} ·{" "}
                {t.pointCount.toLocaleString()} pts
                {t.missing && <span className="missing"> · missing</span>}
                {t.edited && <span> · edited</span>}
              </div>
              {t.tags.length > 0 && (
                <div className="track-tags">
                  {t.tags.map((g) => (
                    <span key={g} className="tag small">
                      {g}
                    </span>
                  ))}
                </div>
              )}
            </li>
          ))}
        </ul>
      </section>
    </nav>
  );
}
