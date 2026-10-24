const UNITS = ["KiB", "MiB", "GiB", "TiB"];

export function bytes(n: number): string {
  if (n < 1024) return `${Math.round(n)} B`;
  let v = n / 1024;
  let unit = 0;
  while (v >= 1024 && unit + 1 < UNITS.length) {
    v /= 1024;
    unit++;
  }
  return `${v.toFixed(1)} ${UNITS[unit]}`;
}

export function count(n: number): string {
  return Math.round(n).toLocaleString("en-US");
}

export function percent(part: number, whole: number): string {
  if (whole <= 0) return "";
  const p = (part / whole) * 100;
  if (p >= 10) return `${p.toFixed(0)}%`;
  if (p >= 0.1) return `${p.toFixed(1)}%`;
  return p > 0 ? "<0.1%" : "0%";
}

export function duration(ns: number): string {
  if (ns < 1e6) return `${(ns / 1e3).toFixed(0)} µs`;
  if (ns < 1e9) return `${(ns / 1e6).toFixed(1)} ms`;
  return `${(ns / 1e9).toFixed(2)} s`;
}
