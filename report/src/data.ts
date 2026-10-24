// The data embedded in the page, and the call tree built from it.
// The shape is written by crates/heapscope-report/src/lib.rs.

export interface Dropped {
  overflow: number;
  min_lifetime: number;
  sampling: number;
}

export interface CaptureInfo {
  mode: "full" | "sampled" | "attached";
  sample_rate: number;
  min_lifetime_ms: number;
  attached: boolean;
  estimated: boolean;
  complete: boolean;
  exe: string;
  command_line: string;
  pid: number;
  pointer_size: number;
  dropped: Dropped;
}

export interface Site {
  id: string;
  function: string;
  module: string | null;
  file: string | null;
  line: number;
  path: string[];
  allocs: number;
  alloc_bytes: number;
  frees: number;
  freed_bytes: number;
  live_allocs: number;
  live_bytes: number;
  peak_live_bytes: number;
  short_lived_allocs: number;
}

export interface LeakEntry {
  id: string;
  function: string;
  module: string | null;
  file: string | null;
  line: number;
  path: string[];
  kind: "heap" | "pool" | "virtual" | "handle";
  allocs: number;
  bytes: number;
  suppressed_by?: { rule: string; reason: string; builtin: boolean };
}

export interface Leaks {
  kind: "leaks" | "net_growth";
  title: string;
  notes: string[];
  total_allocs: number;
  total_bytes: number;
  suppressed_allocs: number;
  suppressed_bytes: number;
  by_kind: { kind: string; allocs: number; bytes: number }[];
  entries: LeakEntry[];
  suppressed: LeakEntry[];
}

export interface Churn {
  file: string | null;
  line: number;
  function: string;
  module: string | null;
  path: string[];
  allocs: number;
  bytes: number;
  avg_lifetime_ns: number;
  share: number;
}

export interface Marker {
  label: string;
  ts_ns: number;
  live_bytes: number;
  live_allocs: number;
}

export interface Summary {
  schema_version: number;
  capture: CaptureInfo;
  totals: {
    allocs: number;
    alloc_bytes: number;
    frees: number;
    freed_bytes: number;
    live_allocs: number;
    live_bytes: number;
    unmatched_frees: number;
    missed_frees: number;
  };
  peak: { ts_ns: number; live_bytes: number; live_allocs: number };
  sites: Site[];
  leaks: Leaks;
  churn: Churn[];
  markers: Marker[];
  warnings: string[];
}

/** Columns indexed by node id; node 0 is the root and parents have lower ids than children. */
interface TreeColumns {
  /** [function, module, file, line] */
  frames: [string, string | null, string | null, number][];
  frame: number[];
  parent: number[];
  allocs: number[];
  alloc_bytes: number[];
  live_allocs: number[];
  live_bytes: number[];
  peak_bytes: number[];
}

export interface ReportData {
  report_version: number;
  summary: Summary;
  tree: TreeColumns;
  /** [ts_ns, live_bytes, live_allocs] */
  timeline: [number, number, number][];
}

export const SUPPORTED_VERSION = 1;

/** The embedded data, or why there is none. */
export function load(): { data: ReportData } | { error: string } {
  const el = document.getElementById("hs-data");
  const text = el?.textContent?.trim() ?? "";
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch {
    return { error: "This page has no data. It is the empty template; generate a report with `heapscope report`." };
  }
  const d = parsed as Partial<ReportData> | null;
  if (!d || typeof d !== "object" || d.report_version !== SUPPORTED_VERSION || !d.summary || !d.tree) {
    return {
      error: `Unsupported report data (version ${String(d?.report_version)}); this page reads version ${SUPPORTED_VERSION}.`,
    };
  }
  return { data: d as ReportData };
}

export type Metric = "alloc_bytes" | "allocs" | "live_bytes" | "peak_bytes";

export const METRICS: { id: Metric; label: string; unit: "bytes" | "count" }[] = [
  { id: "alloc_bytes", label: "Bytes allocated", unit: "bytes" },
  { id: "allocs", label: "Allocation count", unit: "count" },
  { id: "live_bytes", label: "Live at end", unit: "bytes" },
  { id: "peak_bytes", label: "Live at peak", unit: "bytes" },
];

export function metricInfo(m: Metric) {
  return METRICS.find((x) => x.id === m) ?? METRICS[0]!;
}

/** The call tree with own and inclusive figures per metric. */
export class Tree {
  readonly size: number;
  readonly parent: Uint32Array;
  readonly children: number[][];
  readonly own: Record<Metric, Float64Array>;
  readonly incl: Record<Metric, Float64Array>;
  private sorted = new Map<Metric, number[][]>();

  constructor(private readonly cols: TreeColumns) {
    const n = cols.parent.length;
    this.size = n;
    this.parent = Uint32Array.from(cols.parent);
    this.children = Array.from({ length: n }, () => []);
    for (let i = 1; i < n; i++) this.children[this.parent[i]!]!.push(i);
    this.own = {
      alloc_bytes: Float64Array.from(cols.alloc_bytes),
      allocs: Float64Array.from(cols.allocs),
      live_bytes: Float64Array.from(cols.live_bytes),
      peak_bytes: Float64Array.from(cols.peak_bytes),
    };
    const incl = (own: Float64Array) => {
      const out = Float64Array.from(own);
      // Children have higher ids than their parent, so one backwards sweep totals every subtree.
      for (let i = n - 1; i >= 1; i--) out[this.parent[i]!]! += out[i]!;
      return out;
    };
    this.incl = {
      alloc_bytes: incl(this.own.alloc_bytes),
      allocs: incl(this.own.allocs),
      live_bytes: incl(this.own.live_bytes),
      peak_bytes: incl(this.own.peak_bytes),
    };
  }

  name(i: number): string {
    return this.cols.frames[this.cols.frame[i]!]![0];
  }
  module(i: number): string | null {
    return this.cols.frames[this.cols.frame[i]!]![1];
  }
  file(i: number): string | null {
    return this.cols.frames[this.cols.frame[i]!]![2];
  }
  line(i: number): number {
    return this.cols.frames[this.cols.frame[i]!]![3];
  }
  location(i: number): string | null {
    const f = this.file(i);
    if (!f) return null;
    const l = this.line(i);
    return l > 0 ? `${f}:${l}` : f;
  }

  /** Children of every node, largest first by the inclusive figure of `metric`. */
  childrenBy(metric: Metric): number[][] {
    let s = this.sorted.get(metric);
    if (!s) {
      const v = this.incl[metric];
      s = this.children.map((c) => (c.length > 1 ? [...c].sort((a, b) => v[b]! - v[a]! || a - b) : c));
      this.sorted.set(metric, s);
    }
    return s;
  }

  /** Root first, then down to `node`. */
  ancestry(node: number): number[] {
    const out: number[] = [];
    for (let i = node; ; i = this.parent[i]!) {
      out.push(i);
      if (i === 0) break;
    }
    return out.reverse();
  }
}
