import { useEffect } from "react";
import { api, type PointTable } from "../api";
import { bboxOf, deletePoints, pointsLabel } from "../pointEditing";

interface Props {
  x: number;
  y: number;
  trackId: number;
  table: PointTable;
  /** Selected indices, ascending. */
  selection: number[];
  onClose: () => void;
  onZoomTo: (bbox: [number, number, number, number]) => void;
  onEdited: () => void;
  onError: (message: string) => void;
}

/** Context menu for selected points; opened from the tree or the map. */
export function PointMenu({ x, y, trackId, table, selection, onClose, onZoomTo, onEdited, onError }: Props) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("mousedown", onClose);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("mousedown", onClose);
      window.removeEventListener("keydown", onKey);
    };
  }, [onClose]);

  const run = async (action: () => Promise<boolean | unknown>) => {
    onClose();
    try {
      if ((await action()) !== false) onEdited();
    } catch (e) {
      onError(String(e));
    }
  };

  const splitAt = selection[0];
  const canSplit = selection.length > 0 && splitAt > 0 && splitAt < table.length;

  return (
    <div className="context-menu" style={{ left: x, top: y }} onMouseDown={(e) => e.stopPropagation()}>
      <button disabled={!canSplit} onClick={() => run(() => api.splitTrack(trackId, splitAt))}>
        Split track here (new track starts at #{(splitAt ?? 0) + 1})
      </button>
      <button
        disabled={!selection.length}
        onClick={() => run(() => deletePoints(trackId, selection, pointsLabel(selection.length)))}
      >
        Delete {selection.length === 1 ? "point" : pointsLabel(selection.length)}
      </button>
      <hr />
      <button
        disabled={!selection.length}
        onClick={() => {
          onClose();
          const b = bboxOf(table, selection);
          if (b) onZoomTo(b);
        }}
      >
        Zoom to selection
      </button>
    </div>
  );
}
