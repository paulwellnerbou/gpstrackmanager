import { useState, type KeyboardEvent, type PointerEvent } from "react";

interface Props {
  /** Which pane the handle resizes; a right pane grows when dragged leftwards. */
  side: "left" | "right";
  /** CSS custom property on the parent that holds the pane width. */
  cssVar: string;
  width: number;
  defaultWidth: number;
  min: number;
  /** Evaluated per gesture so it follows the window and the other pane. */
  max: () => number;
  onChange: (width: number) => void;
}

export function PaneResizer(p: Props) {
  const [dragging, setDragging] = useState(false);
  const clamp = (w: number) => Math.round(Math.max(p.min, Math.min(Math.max(p.min, p.max()), w)));

  const onPointerDown = (e: PointerEvent<HTMLDivElement>) => {
    if (e.button !== 0) return;
    e.preventDefault();
    const handle = e.currentTarget;
    const container = handle.parentElement;
    if (!container) return;
    handle.setPointerCapture(e.pointerId);
    setDragging(true);
    const startX = e.clientX;
    let width = p.width;
    // Writing the CSS variable directly avoids re-rendering the whole app (track list,
    // map, point tree) on every pointer move; React gets the final width on release.
    const move = (ev: globalThis.PointerEvent) => {
      const dx = ev.clientX - startX;
      width = clamp(p.width + (p.side === "left" ? dx : -dx));
      container.style.setProperty(p.cssVar, `${width}px`);
    };
    const up = () => {
      handle.removeEventListener("pointermove", move);
      handle.removeEventListener("pointerup", up);
      handle.removeEventListener("pointercancel", up);
      setDragging(false);
      p.onChange(width);
    };
    handle.addEventListener("pointermove", move);
    handle.addEventListener("pointerup", up);
    handle.addEventListener("pointercancel", up);
  };

  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    const step = e.shiftKey ? 64 : 16;
    const grow = p.side === "left" ? "ArrowRight" : "ArrowLeft";
    const shrink = p.side === "left" ? "ArrowLeft" : "ArrowRight";
    if (e.key === grow) p.onChange(clamp(p.width + step));
    else if (e.key === shrink) p.onChange(clamp(p.width - step));
    else return;
    e.preventDefault();
  };

  return (
    <div
      className={`pane-resizer ${p.side} ${dragging ? "dragging" : ""}`}
      role="separator"
      aria-orientation="vertical"
      aria-valuenow={p.width}
      aria-valuemin={p.min}
      tabIndex={0}
      title="Drag to resize, double-click to reset"
      onPointerDown={onPointerDown}
      onDoubleClick={() => p.onChange(clamp(p.defaultWidth))}
      onKeyDown={onKeyDown}
    />
  );
}
