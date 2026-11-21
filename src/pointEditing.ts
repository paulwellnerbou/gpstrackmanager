import { ask } from "@tauri-apps/plugin-dialog";
import type { PointTable } from "./api";

/** Selected point indices of the selected track, shared by the tree and the map. */
export interface PointSelection {
  set: Set<number>;
  /** Last plainly clicked point; ⇧-click selects from here. */
  anchor: number | null;
}

export const EMPTY_SELECTION: PointSelection = { set: new Set(), anchor: null };

export interface Modifiers {
  shift: boolean;
  /** ⌘ on macOS, Ctrl elsewhere. */
  toggle: boolean;
}

/** Finder-style click: plain replaces, ⌘ toggles, ⇧ extends from the anchor (⌘⇧ adds). */
export function clickSelection(cur: PointSelection, i: number, m: Modifiers): PointSelection {
  if (m.shift && cur.anchor != null) {
    const [a, b] = [Math.min(cur.anchor, i), Math.max(cur.anchor, i)];
    const set = m.toggle ? new Set(cur.set) : new Set<number>();
    for (let k = a; k <= b; k++) set.add(k);
    return { set, anchor: cur.anchor };
  }
  if (m.toggle) {
    const set = new Set(cur.set);
    if (set.has(i)) set.delete(i);
    else set.add(i);
    return { set, anchor: i };
  }
  return { set: new Set([i]), anchor: i };
}

export function rangeSelection(start: number, end: number): PointSelection {
  const set = new Set<number>();
  for (let k = start; k < end; k++) set.add(k);
  return { set, anchor: start };
}

export function bboxOf(table: PointTable, indices: Iterable<number>): [number, number, number, number] | null {
  let [x0, y0, x1, y1] = [Infinity, Infinity, -Infinity, -Infinity];
  for (const i of indices) {
    x0 = Math.min(x0, table.lon[i]);
    x1 = Math.max(x1, table.lon[i]);
    y0 = Math.min(y0, table.lat[i]);
    y1 = Math.max(y1, table.lat[i]);
  }
  return Number.isFinite(x0) ? [x0, y0, x1, y1] : null;
}

/** Asks before deleting more than one point. */
export async function confirmDelete(count: number, what: string): Promise<boolean> {
  if (count === 0) return false;
  if (count === 1) return true;
  return ask(`Delete ${what} from this track?`, { title: "Delete points", kind: "warning", okLabel: "Delete" });
}

const isMac = /Mac/i.test(navigator.userAgent);

/** ⌘ toggles on macOS, where Ctrl-click is the secondary click; Ctrl elsewhere. */
export function isToggleKey(e: { metaKey: boolean; ctrlKey: boolean }) {
  return isMac ? e.metaKey : e.ctrlKey;
}

/** Ctrl-click on macOS opens the context menu and must not change the selection. */
export function isSecondaryClick(e: { button: number; ctrlKey: boolean }) {
  return e.button !== 0 || (isMac && e.ctrlKey);
}

export function pointsLabel(count: number) {
  return count === 1 ? "1 point" : `${count.toLocaleString()} points`;
}
