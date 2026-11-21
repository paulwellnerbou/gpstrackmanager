import { useEffect, useState } from "react";

const query = "(prefers-color-scheme: dark)";

/** The OS light/dark setting, updated live when the user switches it. */
export function useColorScheme(): "light" | "dark" {
  const [dark, setDark] = useState(() => window.matchMedia(query).matches);
  useEffect(() => {
    const mq = window.matchMedia(query);
    const onChange = () => setDark(mq.matches);
    mq.addEventListener("change", onChange);
    return () => mq.removeEventListener("change", onChange);
  }, []);
  return dark ? "dark" : "light";
}
