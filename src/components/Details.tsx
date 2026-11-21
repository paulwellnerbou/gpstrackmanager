import { useEffect, useState } from "react";
import {
  ACTIVITIES,
  api,
  displayName,
  formatDate,
  formatDistance,
  formatDuration,
  type PointTable,
  type LastEdit,
  type TrackSummary,
  type WaypointRow,
} from "../api";
import { TrackTree } from "./TrackTree";
import { CleanupPanel } from "./CleanupPanel";
import { EMPTY_SELECTION, type PointSelection } from "../pointEditing";

interface Props {
  track: TrackSummary;
  allTags: string[];
  onSaveMeta: (title: string, description: string) => void;
  onSetTags: (tags: string[]) => void;
  onSetActivity: (activity: string | null) => void;
  table: PointTable | null;
  fileTracks: TrackSummary[];
  waypoints: WaypointRow[];
  focusedWaypoint: number | null;
  onSelectTrack: (id: number) => void;
  onFocusWaypoint: (id: number | null) => void;
  onHover: (lonLat: [number, number] | null) => void;
  selection: PointSelection;
  onSelectionChange: (s: PointSelection) => void;
  reveal: { index: number; seq: number } | null;
  locked: boolean;
  onBeginEdit: () => void;
  onZoomTo: (bbox: [number, number, number, number]) => void;
  onPanTo: (lonLat: [number, number]) => void;
  onEdited: () => void;
  onError: (message: string) => void;
  /** Newest point edit of the track's file, if any. */
  lastEdit: LastEdit | null;
  onUndo: () => void;
}

export function Details({ track, allTags, onSaveMeta, onSetTags, onSetActivity, lastEdit, onUndo, ...edit }: Props) {
  const [title, setTitle] = useState(track.title ?? "");
  const [description, setDescription] = useState(track.description ?? "");
  const [tagInput, setTagInput] = useState("");
  const [cleaningUp, setCleaningUp] = useState(false);

  useEffect(() => {
    setTitle(track.title ?? "");
    setDescription(track.description ?? "");
    setTagInput("");
  }, [track.id, track.title, track.description]);

  useEffect(() => setCleaningUp(false), [track.id]);

  const save = () => {
    if (title !== (track.title ?? "") || description !== (track.description ?? "")) onSaveMeta(title, description);
  };

  const addTag = () => {
    const tag = tagInput.trim();
    if (tag && !track.tags.some((t) => t.toLowerCase() === tag.toLowerCase())) onSetTags([...track.tags, tag]);
    setTagInput("");
  };

  const call = (f: () => Promise<unknown>) => f().catch((e) => edit.onError(String(e)));

  const fallbackName = track.title ? (track.name ?? "") : displayName(track);

  return (
    <aside className="details">
      <div className="details-top">
        <div className="open-in">
          <button onClick={() => call(() => api.openInGoogleEarth(track.id))} title="Export as KML and open it">
            Google Earth
          </button>
          <button onClick={() => call(() => api.openInMaps(track.id))} title="Open the track's area in Maps">
            Maps
          </button>
          <button onClick={() => call(() => api.revealTrackFile(track.id))} title="Show the source file in Finder">
            Show file
          </button>
        </div>
        {cleaningUp ? (
          <CleanupPanel
            trackId={track.id}
            table={edit.table}
            locked={edit.locked}
            onSelectionChange={edit.onSelectionChange}
            onZoomTo={edit.onZoomTo}
            onBeginEdit={edit.onBeginEdit}
            onEdited={edit.onEdited}
            onError={edit.onError}
            onClose={() => {
              setCleaningUp(false);
              edit.onSelectionChange(EMPTY_SELECTION);
            }}
          />
        ) : (
          <div className="track-actions">
            <button
              onClick={() => setCleaningUp(true)}
              title="Remove the scatter where the logger lay still, e.g. left on indoors"
            >
              Clean up track…
            </button>
          </div>
        )}
        <label>
          Title
          <input value={title} placeholder={fallbackName} onChange={(e) => setTitle(e.target.value)} onBlur={save} />
        </label>
        <label>
          Activity
          <select value={track.activity ?? ""} onChange={(e) => onSetActivity(e.target.value || null)}>
            <option value="">{fileActivityLabel(track.fileActivity)}</option>
            {ACTIVITIES.map((a) => (
              <option key={a.key} value={a.key}>
                {a.icon} {a.label}
              </option>
            ))}
          </select>
        </label>
        <label>
          Description
          <textarea
            rows={3}
            value={description}
            placeholder={track.fileDescription ?? "Add a description…"}
            onChange={(e) => setDescription(e.target.value)}
            onBlur={save}
          />
        </label>
        <div className="field">
          Tags
          <div className="tags">
            {track.tags.map((t) => (
              <span key={t} className="tag">
                {t}
                <button title="Remove tag" onClick={() => onSetTags(track.tags.filter((x) => x !== t))}>
                  ×
                </button>
              </span>
            ))}
            <input
              className="tag-input"
              list="all-tags"
              value={tagInput}
              placeholder="Add tag…"
              onChange={(e) => setTagInput(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" || e.key === ",") {
                  e.preventDefault();
                  addTag();
                }
              }}
              onBlur={addTag}
            />
            <datalist id="all-tags">
              {allTags.map((t) => (
                <option key={t} value={t} />
              ))}
            </datalist>
          </div>
        </div>
        <dl className="stats">
          <dt>Start</dt>
          <dd>{formatDate(track.startTime)}</dd>
          <dt>Duration</dt>
          <dd>{formatDuration(track.startTime, track.endTime)}</dd>
          <dt>Distance</dt>
          <dd>{formatDistance(track.distanceM)}</dd>
          <dt>Points</dt>
          <dd>{track.pointCount.toLocaleString()}</dd>
          <dt>Elevation</dt>
          <dd>
            {track.minEle != null && track.maxEle != null
              ? `${Math.round(track.minEle)} – ${Math.round(track.maxEle)} m`
              : "—"}
          </dd>
          <dt>File</dt>
          <dd className="path" title={track.filePath}>
            {track.filePath}
            {track.idx < 0
              ? " (part split off in the catalog)"
              : track.trackCountInFile > 1 && ` (track ${track.idx + 1} of ${track.trackCountInFile})`}
            {track.missing && <strong className="missing"> missing</strong>}
          </dd>
        </dl>
        {(track.edited || lastEdit) && (
          <div className="edited-note">
            Points edited in the catalog; the file is unchanged.{" "}
            {lastEdit && (
              <>
                <button
                  className="link"
                  disabled={edit.locked}
                  onClick={onUndo}
                  title={`Undo the newest point edit of this file (⌘Z), from ${formatDate(lastEdit.createdAt)}`}
                >
                  Undo {lastEdit.label.charAt(0).toLowerCase() + lastEdit.label.slice(1)}
                </button>
                {" · "}
              </>
            )}
            <button
              className="link"
              onClick={() =>
                call(async () => {
                  edit.onBeginEdit();
                  try {
                    await api.revertFile(track.fileId);
                  } finally {
                    edit.onEdited();
                  }
                })
              }
              title="Discard all point edits and split-off parts of this file"
            >
              Revert to file
            </button>
          </div>
        )}
      </div>
      <TrackTree track={track} {...edit} />
    </aside>
  );
}

function fileActivityLabel(key: string | null) {
  const a = ACTIVITIES.find((x) => x.key === key);
  return a ? `${a.icon} ${a.label} (from file)` : "Not set";
}
