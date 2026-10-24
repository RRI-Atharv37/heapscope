import type { View } from "../main";
import type { Churn, LeakEntry, Site, Summary } from "../data";
import { append, h, type Child } from "../dom";
import { bytes, count, duration } from "../format";

const TOP = 10;

function card(label: string, value: string, sub: string): HTMLElement {
  return h(
    "div",
    { class: "card" },
    h("div", { class: "label" }, label),
    h("div", { class: "value" }, value),
    h("div", { class: "sub" }, sub),
  );
}

function table(head: [string, boolean][], rows: Child[][]): HTMLElement {
  return h(
    "div",
    { class: "scroll-x" },
    h(
      "table",
      { class: "data" },
      h("thead", null, h("tr", null, ...head.map(([t, num]) => h("th", { class: num ? "num" : null }, t)))),
      h(
        "tbody",
        null,
        ...rows.map((cells) =>
          h("tr", null, ...cells.map((c, i) => h("td", { class: head[i]![1] ? "num" : i === cells.length - 1 ? "site" : null }, c))),
        ),
      ),
    ),
  );
}

/** Function, where it is, and the callers that lead to it. */
function siteCell(s: { function: string; module: string | null; file: string | null; line: number; path: string[] }): Child[] {
  const where = s.file ? (s.line > 0 ? `${s.file}:${s.line}` : s.file) : s.module;
  const callers = s.path.slice(1, 5);
  return [
    h("code", null, s.function),
    where ? h("span", { class: "loc" }, where) : null,
    callers.length ? h("span", { class: "loc" }, "from " + callers.join(" ← ")) : null,
  ];
}

function topSites(sites: Site[], estimated: boolean): HTMLElement {
  const rows = sites.slice(0, TOP).map((s) => [
    bytes(s.alloc_bytes),
    count(s.allocs),
    bytes(s.live_bytes),
    bytes(s.peak_live_bytes),
    siteCell(s),
  ]);
  return h(
    "section",
    null,
    h("h2", null, "Top call paths by bytes allocated", estimated ? " (estimated)" : ""),
    sites.length === 0
      ? h("p", { class: "muted" }, "No allocations were recorded.")
      : table(
          [
            ["Allocated", true],
            ["Allocations", true],
            ["Live at end", true],
            ["Live at peak", true],
            ["Allocation site", false],
          ],
          rows,
        ),
    sites.length > TOP ? h("p", { class: "muted" }, `${count(sites.length - TOP)} more in the Call tree view.`) : null,
  );
}

function leakRow(e: LeakEntry): Child[] {
  const size = e.kind === "handle" ? `${count(e.allocs)} handles` : bytes(e.bytes);
  return [
    h("span", { class: "pill" }, e.kind),
    size,
    e.kind === "handle" ? "" : count(e.allocs),
    siteCell(e),
  ];
}

function leaks(l: Summary["leaks"]): HTMLElement {
  const head: [string, boolean][] = [
    ["Kind", false],
    ["Size", true],
    ["Allocations", true],
    ["Allocation site", false],
  ];
  return h(
    "section",
    null,
    h("h2", null, l.title),
    ...l.notes.map((n) => h("p", { class: "muted" }, n)),
    l.entries.length === 0
      ? h("p", null, "None.")
      : [
          h(
            "p",
            null,
            h("strong", null, bytes(l.total_bytes)),
            ` in ${count(l.total_allocs)} allocations over ${count(l.entries.length)} call paths. `,
            ...l.by_kind.flatMap((k) => [h("span", { class: "pill" }, `${k.kind} ${k.kind === "handle" ? count(k.allocs) : bytes(k.bytes)}`), " "]),
          ),
          table(head, l.entries.slice(0, TOP).map(leakRow)),
          l.entries.length > TOP ? h("p", { class: "muted" }, `${count(l.entries.length - TOP)} more candidates not shown.`) : null,
        ],
    l.suppressed.length > 0
      ? h(
          "details",
          null,
          h("summary", null, `${count(l.suppressed.length)} suppressed call paths (${bytes(l.suppressed_bytes)})`),
          table(
            [...head, ["Suppressed by", false]],
            l.suppressed.slice(0, 50).map((e) => [...leakRow(e).slice(0, 3), siteCell(e), e.suppressed_by?.reason ?? ""]),
          ),
        )
      : null,
  );
}

function churn(rows: Churn[]): HTMLElement | null {
  if (rows.length === 0) return null;
  return h(
    "section",
    null,
    h("h2", null, "Short-lived allocations"),
    table(
      [
        ["Allocations", true],
        ["Bytes", true],
        ["Of the site", true],
        ["Average life", true],
        ["Allocation site", false],
      ],
      rows
        .slice(0, 5)
        .map((c) => [count(c.allocs), bytes(c.bytes), `${(c.share * 100).toFixed(0)}%`, duration(c.avg_lifetime_ns), siteCell(c)]),
    ),
  );
}

export const summaryView: View = (host, ctx) => {
  const s = ctx.data.summary;
  const t = s.totals;
  const c = s.capture;
  const est = c.estimated ? "estimated" : "";
  append(host, [
    h(
      "div",
      { class: "cards" },
      card("Allocated", bytes(t.alloc_bytes), `${count(t.allocs)} allocations ${est}`.trim()),
      card("Freed", bytes(t.freed_bytes), `${count(t.frees)} frees`),
      card("Live at end", bytes(t.live_bytes), `${count(t.live_allocs)} allocations`),
      card("Peak live", bytes(s.peak.live_bytes), `at ${duration(s.peak.ts_ns)}, ${count(s.peak.live_allocs)} allocations`),
    ),
    topSites(s.sites, c.estimated),
    leaks(s.leaks),
    churn(s.churn),
    s.markers.length
      ? h(
          "section",
          null,
          h("h2", null, "Markers"),
          table(
            [
              ["Time", true],
              ["Live", true],
              ["Allocations", true],
              ["Label", false],
            ],
            s.markers.map((m) => [duration(m.ts_ns), bytes(m.live_bytes), count(m.live_allocs), m.label]),
          ),
        )
      : null,
    h(
      "section",
      null,
      h("h2", null, "Capture"),
      h(
        "dl",
        { class: "kv" },
        ...[
          ["Program", c.command_line || c.exe],
          ["Process", `${c.pid}, ${c.pointer_size * 8}-bit`],
          ["Mode", c.mode === "sampled" ? `sampled, 1 in ${c.sample_rate}` : c.mode],
          ["Frees of unseen memory", count(t.unmatched_frees)],
          ["Allocations at a live address", count(t.missed_frees)],
          ["Dropped", `${count(c.dropped.overflow)} lost to overflow, ${count(c.dropped.min_lifetime)} folded by min-lifetime, ${count(c.dropped.sampling)} skipped by sampling`],
        ].flatMap(([k, v]) => [h("dt", null, k), h("dd", null, v)]),
      ),
      s.warnings.length
        ? h("details", null, h("summary", null, `${s.warnings.length} warnings`), h("ul", null, ...s.warnings.map((w) => h("li", null, w))))
        : null,
    ),
  ]);
};
