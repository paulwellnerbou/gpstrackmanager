import { useEffect, useLayoutEffect, useRef, useState, type CSSProperties, type ReactNode } from "react";
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
  /** Playing the exit animation; the menu no longer reacts. */
  closing?: boolean;
}

/** Context menu for selected points; opened from the tree or the map. */
export function PointMenu(props: Props) {
  const { x, y, trackId, table, selection, onClose, onZoomTo, onBeginEdit, onEdited, onError, closing } = props;
  useEffect(() => {
    if (closing) return;
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("mousedown", onClose);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("mousedown", onClose);
      window.removeEventListener("keydown", onKey);
    };
  }, [onClose, closing]);

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
    <MenuBox x={x} y={y} closing={closing}>
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
export function MenuBox({ x, y, closing, children }: { x: number; y: number; closing?: boolean; children: ReactNode }) {
  const ref = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState<CSSProperties>({ left: x, top: y });
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    // Layout size: the bounding rect would include the entry animation's scale.
    const { offsetWidth: width, offsetHeight: height } = el;
    const margin = 4;
    const flipped = y + height > window.innerHeight - margin;
    const left = Math.max(margin, Math.min(x, window.innerWidth - width - margin));
    const top = Math.max(margin, Math.min(flipped ? y - height : y, window.innerHeight - height - margin));
    // Grow out of the click point, wherever the menu ended up relative to it.
    setPos({ left, top, transformOrigin: `${x - left}px ${y - top}px` });
  }, [x, y]);
  return (
    <div
      ref={ref}
      className={closing ? "context-menu closing" : "context-menu"}
      style={pos}
      onMouseDown={(e) => e.stopPropagation()}
    >
      {children}
    </div>
  );
}
