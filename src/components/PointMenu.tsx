import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { api, type PointTable } from "../api";
import { bboxOf, confirmDelete, pointsLabel } from "../pointEditing";

interface Props {
  x: number;
  y: number;
  trackId: number;
  table: PointTable;
  /** Selected indices, ascending. */
  selection: number[];
  onClose: () => void;
  onZoomTo: (bbox: [number, number, number, number]) => void;
  /** Called right before an edit is sent; the selection's indices become invalid. */
  onBeginEdit: () => void;
  onEdited: () => void;
  onError: (message: string) => void;
}

/** Context menu for selected points; opened from the tree or the map. */
export function PointMenu(props: Props) {
  const { x, y, trackId, table, selection, onClose, onZoomTo, onBeginEdit, onEdited, onError } = props;
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("mousedown", onClose);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("mousedown", onClose);
      window.removeEventListener("keydown", onKey);
    };
  }, [onClose]);

  const run = async (action: () => Promise<unknown>) => {
    onClose();
    onBeginEdit();
    try {
      await action();
      onEdited();
    } catch (e) {
      onError(String(e));
    }
  };

  const remove = async () => {
    onClose();
    if (await confirmDelete(selection.length, pointsLabel(selection.length))) {
      await run(() => api.deleteTrackPoints(trackId, selection));
    }
  };

  const splitAt = selection[0];
  // Both parts keep at least two points: a one-point track is no line.
  const canSplit = selection.length > 0 && splitAt >= 2 && splitAt <= table.length - 2;

  return (
    <MenuBox x={x} y={y}>
      <button disabled={!canSplit} onClick={() => run(() => api.splitTrack(trackId, splitAt))}>
        Split track here (new track starts at #{(splitAt ?? 0) + 1})
      </button>
      <button disabled={!selection.length} onClick={remove}>
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
    </MenuBox>
  );
}

/** A context menu at a click position, moved or flipped so it stays inside the window. */
export function MenuBox({ x, y, children }: { x: number; y: number; children: ReactNode }) {
  const ref = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState({ left: x, top: y });
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const { width, height } = el.getBoundingClientRect();
    const margin = 4;
    const top = y + height > window.innerHeight - margin ? y - height : y;
    setPos({
      left: Math.max(margin, Math.min(x, window.innerWidth - width - margin)),
      top: Math.max(margin, Math.min(top, window.innerHeight - height - margin)),
    });
  }, [x, y]);
  return (
    <div ref={ref} className="context-menu" style={pos} onMouseDown={(e) => e.stopPropagation()}>
      {children}
    </div>
  );
}
