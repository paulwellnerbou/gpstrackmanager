import { useEffect, useRef, useState, type ReactNode } from "react";

/** Matches `--exit` in App.css. */
export const EXIT_MS = 120;

const reducedMotion = () => window.matchMedia?.("(prefers-reduced-motion: reduce)").matches ?? false;

/**
 * Keeps the last present `value` (anything but null, undefined or false) while its exit
 * animation plays, so a popover or panel can animate out instead of vanishing.
 * `closing` is true during that time.
 */
export function usePresence<T>(value: T | null | undefined | false, ms = EXIT_MS) {
  const present = value != null && value !== false;
  const last = useRef<T | null>(null);
  if (present) last.current = value as T;
  const [gone, setGone] = useState(!present);
  if (present && gone) setGone(false);

  useEffect(() => {
    if (present || gone) return;
    const t = window.setTimeout(() => setGone(true), reducedMotion() ? 0 : ms);
    return () => window.clearTimeout(t);
  }, [present, gone, ms]);

  return { shown: present ? (value as T) : gone ? null : last.current, closing: !present && !gone };
}

/**
 * Shows or hides its children by animating their height and opacity, so the content
 * around them slides instead of jumping.
 */
export function Collapse({ show, children, className }: { show: boolean; children: ReactNode; className?: string }) {
  const { shown, closing } = usePresence(show);
  // During the exit, keep rendering what was last shown rather than the hidden state's children.
  const last = useRef(children);
  if (show) last.current = children;
  if (!shown) return null;
  return (
    <div className={`collapse${closing ? " closing" : ""}${className ? ` ${className}` : ""}`}>
      <div>{show ? children : last.current}</div>
    </div>
  );
}
