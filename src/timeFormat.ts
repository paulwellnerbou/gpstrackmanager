const pad = (n: number) => String(n).padStart(2, "0");
const dayFormat = new Intl.DateTimeFormat(undefined, { day: "2-digit", month: "2-digit" });

const dayKey = (ms: number) => {
  const d = new Date(ms);
  return d.getFullYear() * 10000 + d.getMonth() * 100 + d.getDate();
};

const spanCache = new WeakMap<Float64Array, boolean>();

/** True when the known times (NaN for unknown) fall on more than one local calendar day. */
export function spansDays(time: Float64Array): boolean {
  let cached = spanCache.get(time);
  if (cached == null) {
    let lo = Infinity;
    let hi = -Infinity;
    for (const t of time) {
      if (t < lo) lo = t;
      if (t > hi) hi = t;
    }
    cached = lo <= hi && dayKey(lo) !== dayKey(hi);
    spanCache.set(time, cached);
  }
  return cached;
}

/** "14:03:22", or "21.11. 14:03:22" (in the locale's day order) when `withDate`. */
export function formatClock(ms: number | null, withDate: boolean, seconds = true): string {
  if (ms == null || Number.isNaN(ms)) return "—";
  const d = new Date(ms);
  const clock = `${pad(d.getHours())}:${pad(d.getMinutes())}${seconds ? `:${pad(d.getSeconds())}` : ""}`;
  return withDate ? `${dayFormat.format(d)} ${clock}` : clock;
}
