// Diff view: the call tree of what changed in the live set between two moments, either two
// points of one capture (this report) or two captures (a report from `heapscope diff --html`).
// Filters work on allocation sites, the nodes that hold memory themselves; the tree shows the
// paths down to the sites that pass, with inclusive figures.

import type { Ctx, View } from "../main";
import { Shape, liveAt, moments, type DiffInfo, type Moment } from "../data";
import { append, clear, h } from "../dom";
import { bytes, count, duration, signed } from "../format";

const ROW = 24;
const OVERSCAN = 6;
const MAX_EXPANDED = 1_000_000;
const FIRST_ROWS = 25;
const MORE_ROWS = 100;

export type Change = "grew" | "new" | "shrank" | "gone" | "same";
const CHANGES: { id: Change; label: string; tone: "up" | "down" | "none" }[] = [
  { id: "grew", label: "Grew", tone: "up" },
  { id: "new", label: "New", tone: "up" },
  { id: "shrank", label: "Shrank", tone: "down" },
  { id: "gone", label: "Gone", tone: "down" },
  { id: "same", label: "Unchanged", tone: "none" },
];

type Unit = "bytes" | "allocs";
interface Live {
  allocs: Float64Array;
  bytes: Float64Array;
}

/** A call tree with the live set on two sides, and the inclusive totals of both. */
export class DeltaTree {
  readonly inclBefore: Live;
  readonly inclAfter: Live;
  /** Per node, the change of its own memory; null for a node that holds none on either side. */
  readonly site: (Change | null)[];
  readonly counts: Record<Change, number> = { grew: 0, new: 0, shrank: 0, gone: 0, same: 0 };
  private sorted = new Map<Unit, number[][]>();

  constructor(
    readonly shape: Shape,
    readonly before: Live,
    readonly after: Live,
  ) {
    const n = shape.size;
    const total = (own: Float64Array) => {
      const out = Float64Array.from(own);
      for (let i = n - 1; i >= 1; i--) out[shape.parent[i]!]! += out[i]!;
      return out;
    };
    this.inclBefore = { allocs: total(before.allocs), bytes: total(before.bytes) };
    this.inclAfter = { allocs: total(after.allocs), bytes: total(after.bytes) };
    this.site = new Array<Change | null>(n).fill(null);
    for (let i = 1; i < n; i++) {
      const had = before.bytes[i]! > 0 || before.allocs[i]! > 0;
      const has = after.bytes[i]! > 0 || after.allocs[i]! > 0;
      let c: Change | null = null;
      if (!had && has) c = "new";
      else if (had && !has) c = "gone";
      else if (had && has) {
        const d = after.bytes[i]! - before.bytes[i]! || after.allocs[i]! - before.allocs[i]!;
        c = d > 0 ? "grew" : d < 0 ? "shrank" : "same";
      }
      this.site[i] = c;
      if (c) this.counts[c]++;
    }
  }

  delta(unit: Unit, n: number): number {
    return this.inclAfter[unit][n]! - this.inclBefore[unit][n]!;
  }

  /** Children of every node, the biggest change first. */
  childrenBy(unit: Unit): number[][] {
    let s = this.sorted.get(unit);
    if (!s) {
      s = this.shape.children.map((c) =>
        c.length > 1
          ? [...c].sort(
              (a, b) =>
                Math.abs(this.delta(unit, b)) - Math.abs(this.delta(unit, a)) ||
                this.inclAfter[unit][b]! - this.inclAfter[unit][a]! ||
                a - b,
            )
          : c,
      );
      this.sorted.set(unit, s);
    }
    return s;
  }
}

const fmt = (unit: Unit, v: number): string => (unit === "bytes" ? bytes(v) : count(v));

function percentChange(before: number, after: number): string {
  if (before <= 0) return after > 0 ? "new" : "";
  const p = ((after - before) / before) * 100;
  if (p === 0) return "0%";
  const mag = Math.abs(p) >= 10 ? Math.abs(p).toFixed(0) : Math.abs(p).toFixed(1);
  return `${p > 0 ? "+" : "−"}${mag}%`;
}

const tone = (d: number): string => (d > 0 ? "chg-up" : d < 0 ? "chg-down" : "");

/** The delta tree with its filters. Returns a cleanup function. */
export function deltaTreeView(host: HTMLElement, delta: DeltaTree): () => void {
  const shape = delta.shape;
  if (shape.size <= 1) {
    host.append(h("p", { class: "empty" }, "Nothing was live on either side."));
    return () => {};
  }

  let unit: Unit = "bytes";
  const enabled = new Set<Change>(["grew", "new", "shrank", "gone"]);
  const expanded = new Set<number>();
  let selected = -1;
  let rows: number[] = [];
  /** Node passes the filter itself, or has a descendant that does. */
  let show = new Uint8Array(shape.size);

  function filter(): void {
    show = new Uint8Array(shape.size);
    for (let i = 1; i < shape.size; i++) if (delta.site[i] && enabled.has(delta.site[i]!)) show[i] = 1;
    for (let i = shape.size - 1; i >= 1; i--) if (show[i]) show[shape.parent[i]!] = 1;
  }

  const hasChildren = (n: number): boolean => delta.shape.children[n]!.some((c) => show[c] === 1);
  const visibleKids = (n: number): number[] => delta.childrenBy(unit)[n]!.filter((c) => show[c] === 1);

  function rebuild(): void {
    const out: number[] = [];
    const walk = (n: number) => {
      for (const c of visibleKids(n)) {
        out.push(c);
        if (expanded.has(c)) walk(c);
      }
    };
    walk(0);
    rows = out;
    if (selected >= 0 && !rows.includes(selected)) selected = -1;
    sizer.style.height = `${rows.length * ROW}px`;
    const names = CHANGES.filter((c) => enabled.has(c.id)).map((c) => c.label.toLowerCase());
    status.textContent = rows.length
      ? `${count(rows.length)} rows: the paths to sites that ${names.join(" or ")}.`
      : "No site matches the chosen filters.";
    paint();
  }

  /** Open the biggest changes until a screenful is showing. */
  function openTop(): void {
    expanded.clear();
    rebuild();
    for (let guard = 0; guard < 400 && rows.length < 30; guard++) {
      let best = -1;
      for (const n of rows) {
        if (expanded.has(n) || !hasChildren(n)) continue;
        if (best < 0 || Math.abs(delta.delta(unit, n)) > Math.abs(delta.delta(unit, best))) best = n;
      }
      if (best < 0) break;
      expanded.add(best);
      rebuild();
    }
  }

  function toggle(n: number): void {
    if (!hasChildren(n)) return;
    if (expanded.has(n)) expanded.delete(n);
    else expanded.add(n);
    rebuild();
  }

  function expandAll(): void {
    for (let i = 1; i < Math.min(shape.size, MAX_EXPANDED); i++) if (show[i] && hasChildren(i)) expanded.add(i);
    rebuild();
  }

  // ---- elements ----
  const sizer = h("div", { class: "tree-sizer" });
  const scroller = h("div", { class: "tree-scroll", tabindex: "0", role: "tree", "aria-label": "Delta tree" }, sizer);
  const status = h("div", { class: "tree-status", "aria-live": "polite" });
  const totals = h("p", { class: "totals" });
  const chips = h("div", { class: "chips", role: "group", "aria-label": "Show changes" });

  function drawTotals(): void {
    const b = delta.inclBefore[unit][0]!;
    const a = delta.inclAfter[unit][0]!;
    clear(totals);
    append(totals, [
      `Live ${unit === "bytes" ? "memory" : "allocations"}: ${fmt(unit, b)} → ${fmt(unit, a)} `,
      h("strong", { class: tone(a - b) }, `(${signed(a - b, (v) => fmt(unit, v))})`),
    ]);
  }

  function drawChips(): void {
    clear(chips);
    for (const c of CHANGES) {
      chips.append(
        h(
          "button",
          {
            class: `chip chip-${c.tone}`,
            "aria-pressed": String(enabled.has(c.id)),
            onclick: () => {
              if (enabled.has(c.id)) enabled.delete(c.id);
              else enabled.add(c.id);
              filter();
              drawChips();
              openTop();
              scroller.scrollTop = 0;
            },
          },
          c.label,
          h("span", { class: "muted" }, ` ${count(delta.counts[c.id])}`),
        ),
      );
    }
  }

  const head = h(
    "div",
    { class: "tree-head delta-grid", role: "presentation" },
    h("div", null, h("span", { class: "head-pad" }, "Call path")),
    h("div", { class: "num" }, "Before"),
    h("div", { class: "num" }, "After"),
    h("div", { class: "num" }, "Change"),
    h("div", { class: "num" }, "%"),
  );

  const depthCache = new Map<number, number>();
  function depthOf(n: number): number {
    let d = depthCache.get(n);
    if (d === undefined) {
      d = n === 0 ? -1 : depthOf(shape.parent[n]!) + 1;
      depthCache.set(n, d);
    }
    return d;
  }

  function rowEl(index: number): HTMLElement {
    const n = rows[index]!;
    const before = delta.inclBefore[unit][n]!;
    const after = delta.inclAfter[unit][n]!;
    const d = after - before;
    const open = hasChildren(n) ? expanded.has(n) : null;
    const depth = depthOf(n);
    const p = shape.parent[n]!;
    // Mark where a subtree appears or disappears, not every node below that point.
    const newHere = before <= 0 && after > 0 && (p === 0 || delta.inclBefore[unit][p]! > 0);
    const goneHere = after <= 0 && before > 0 && (p === 0 || delta.inclAfter[unit][p]! > 0);
    const loc = shape.location(n);
    return h(
      "div",
      {
        class: "tree-row delta-grid",
        role: "treeitem",
        "aria-level": depth + 1,
        "aria-expanded": open === null ? null : String(open),
        "aria-selected": n === selected ? "true" : "false",
        style: `top:${index * ROW}px`,
        title: [shape.name(n), shape.module(n), loc].filter(Boolean).join("\n"),
        onclick: () => {
          selected = n;
          toggle(n);
          if (open === null) paint();
          scroller.focus({ preventScroll: true });
        },
      },
      h(
        "div",
        { class: "name", style: `padding-left:${Math.min(depth, 40) * 14}px` },
        h("span", { class: "twisty", "aria-hidden": "true" }, open === null ? "" : open ? "▾" : "▸"),
        h("span", { class: "text" }, shape.name(n)),
        newHere ? h("span", { class: "pill pill-up" }, "new") : null,
        goneHere ? h("span", { class: "pill pill-down" }, "gone") : null,
        shape.module(n) ? h("span", { class: "mod" }, shape.module(n)) : null,
      ),
      h("div", { class: "num" }, fmt(unit, before)),
      h("div", { class: "num" }, fmt(unit, after)),
      h("div", { class: `num ${tone(d)}` }, signed(d, (v) => fmt(unit, v))),
      h("div", { class: `num ${tone(d)}` }, percentChange(before, after)),
    );
  }

  let queued = false;
  function paint(): void {
    queued = false;
    const top = scroller.scrollTop;
    const first = Math.max(0, Math.floor(top / ROW) - OVERSCAN);
    const last = Math.min(rows.length, Math.ceil((top + scroller.clientHeight) / ROW) + OVERSCAN);
    clear(sizer);
    for (let i = first; i < last; i++) sizer.append(rowEl(i));
  }
  scroller.addEventListener("scroll", () => {
    if (!queued) {
      queued = true;
      requestAnimationFrame(paint);
    }
  });
  window.addEventListener("resize", paint);

  function select(index: number): void {
    if (index < 0 || index >= rows.length) return;
    selected = rows[index]!;
    const y = index * ROW;
    if (y < scroller.scrollTop) scroller.scrollTop = y;
    else if (y + ROW > scroller.scrollTop + scroller.clientHeight) scroller.scrollTop = y + ROW - scroller.clientHeight;
    paint();
  }

  scroller.addEventListener("keydown", (e) => {
    const at = rows.indexOf(selected);
    const n = selected;
    switch (e.key) {
      case "ArrowDown":
        select(at < 0 ? 0 : at + 1);
        break;
      case "ArrowUp":
        select(at < 0 ? 0 : at - 1);
        break;
      case "Home":
        select(0);
        break;
      case "End":
        select(rows.length - 1);
        break;
      case "PageDown":
        select(Math.min(rows.length - 1, Math.max(0, at) + Math.floor(scroller.clientHeight / ROW)));
        break;
      case "PageUp":
        select(Math.max(0, Math.max(0, at) - Math.floor(scroller.clientHeight / ROW)));
        break;
      case "ArrowRight":
        if (n >= 0 && hasChildren(n)) {
          if (!expanded.has(n)) toggle(n);
          else select(at + 1);
        }
        break;
      case "ArrowLeft":
        if (n >= 0) {
          if (hasChildren(n) && expanded.has(n)) toggle(n);
          else if (shape.parent[n] !== 0) select(rows.indexOf(shape.parent[n]!));
        }
        break;
      case "Enter":
      case " ":
        if (n >= 0) toggle(n);
        break;
      default:
        return;
    }
    e.preventDefault();
  });

  const unitSelect = h(
    "select",
    {
      onchange: () => {
        unit = unitSelect.value as Unit;
        drawTotals();
        openTop();
        scroller.scrollTop = 0;
      },
    },
    h("option", { value: "bytes" }, "Live bytes"),
    h("option", { value: "allocs" }, "Live allocations"),
  );

  host.append(
    totals,
    h(
      "div",
      { class: "toolbar" },
      h("label", null, "Show", unitSelect),
      chips,
      h("span", { class: "spacer" }),
      h("button", { onclick: expandAll }, "Expand all"),
      h("button", { onclick: () => { expanded.clear(); rebuild(); } }, "Collapse all"),
    ),
    head,
    scroller,
    status,
  );
  filter();
  drawTotals();
  drawChips();
  openTop();
  return () => window.removeEventListener("resize", paint);
}

/** Moments to compare in a capture report, named the way the timeline names them. */
function momentLabel(m: Moment): string {
  const at = duration(m.ts_ns);
  return m.kind === "marker" ? `marker "${m.label}" (at ${at})` : m.kind === "peak" ? `peak (at ${at})` : `end of trace (at ${at})`;
}

/** Diff between two moments of the capture; the timeline can open it with a pair preselected. */
export const diffView: View = (host, ctx: Ctx) => {
  const { data, tree } = ctx;
  const list = moments(data);
  const marks = list.filter((m) => m.kind === "marker");
  const [d0, d1] =
    marks.length >= 2
      ? [marks[0]!, marks[marks.length - 1]!]
      : marks.length === 1
        ? [marks[0]!, list[list.length - 1]!]
        : [list.find((m) => m.kind === "peak")!, list[list.length - 1]!];
  const chosen = (id: string | undefined, fallback: Moment) => list.find((m) => m.id === id) ?? fallback;
  let from = chosen(ctx.state.diff?.from, d0);
  let to = chosen(ctx.state.diff?.to, d1);

  const pick = (current: () => Moment, set: (m: Moment) => void, label: string) => {
    const select = h(
      "select",
      {
        onchange: () => {
          set(list.find((m) => m.id === select.value)!);
          ctx.state.diff = { from: from.id, to: to.id };
          render();
        },
      },
      ...list.map((m) => h("option", { value: m.id, selected: m.id === current().id }, momentLabel(m))),
    );
    return h("label", null, label, select);
  };

  const body = h("div");
  let cleanup: (() => void) | undefined;
  function render(): void {
    cleanup?.();
    clear(body);
    const delta = new DeltaTree(tree, liveAt(data, tree, from), liveAt(data, tree, to));
    cleanup = deltaTreeView(body, delta);
  }

  append(host, [
    h(
      "div",
      { class: "toolbar" },
      pick(() => from, (m) => (from = m), "From"),
      pick(() => to, (m) => (to = m), "To"),
      h("span", { class: "spacer" }),
      h("span", { class: "muted" }, "Compares what was live at two moments of this capture."),
    ),
    body,
  ]);
  render();
  return () => cleanup?.();
};

/** Summary page of a diff report: totals, change counts and the flat list of changed sites. */
export function diffSummary(host: HTMLElement, d: DiffInfo): void {
  const card = (label: string, value: string, sub: string) =>
    h("div", { class: "card" }, h("div", { class: "label" }, label), h("div", { class: "value" }, value), h("div", { class: "sub" }, sub));
  const db = d.after.live_bytes - d.before.live_bytes;
  const da = d.after.live_allocs - d.before.live_allocs;
  const est = d.estimated ? " (estimated)" : "";
  const side = (s: DiffInfo["before"]) => `${s.label}, ${s.at}`;
  let shown = FIRST_ROWS;
  const table = h("div");

  function drawTable(): void {
    clear(table);
    if (d.entries.length === 0) {
      table.append(h("p", { class: "empty" }, "No allocation site changed."));
      return;
    }
    append(table, [
      h(
        "div",
        { class: "scroll-x" },
        h(
          "table",
          { class: "data" },
          h(
            "thead",
            null,
            h("tr", null, h("th", null, "Change"), h("th", { class: "num" }, "Live bytes"), h("th", { class: "num" }, "Allocations"), h("th", null, "Allocation site")),
          ),
          h(
            "tbody",
            null,
            ...d.entries.slice(0, shown).map((e) => {
              const dBytes = e.after[1] - e.before[1];
              const dAllocs = e.after[0] - e.before[0];
              return h(
                "tr",
                null,
                h("td", null, h("span", { class: `pill ${e.change === "new" || e.change === "grew" ? "pill-up" : "pill-down"}` }, e.change)),
                h("td", { class: "num" }, `${bytes(e.before[1])} → ${bytes(e.after[1])}`, h("span", { class: `loc ${tone(dBytes)}` }, signed(dBytes, bytes))),
                h("td", { class: "num" }, `${count(e.before[0])} → ${count(e.after[0])}`, h("span", { class: `loc ${tone(dAllocs)}` }, signed(dAllocs, count))),
                h("td", { class: "site" }, h("code", null, e.path), e.location ? h("span", { class: "loc" }, e.location) : null),
              );
            }),
          ),
        ),
      ),
      d.entries.length > shown
        ? h(
            "p",
            { class: "muted" },
            `Showing ${count(shown)} of ${count(d.entries_total)} sites. `,
            h("button", { onclick: () => { shown += MORE_ROWS; drawTable(); } }, "Show more"),
          )
        : d.entries_total > d.entries.length
          ? h("p", { class: "muted" }, `The ${count(d.entries.length)} biggest of ${count(d.entries_total)} changed sites are listed; the Delta tree has all of them.`)
          : null,
    ]);
  }
  drawTable();

  append(host, [
    h(
      "div",
      { class: "cards" },
      card(`Before${est}`, bytes(d.before.live_bytes), `${count(d.before.live_allocs)} allocations live. ${side(d.before)}`),
      card(`After${est}`, bytes(d.after.live_bytes), `${count(d.after.live_allocs)} allocations live. ${side(d.after)}`),
      h(
        "div",
        { class: "card" },
        h("div", { class: "label" }, "Change in live memory"),
        h("div", { class: `value ${tone(db)}` }, signed(db, bytes)),
        h("div", { class: "sub" }, `${signed(da, count)} allocations`),
      ),
      card("Sites", `${count(d.counts.grew + d.counts.new)} up, ${count(d.counts.shrank + d.counts.gone)} down`, `${d.counts.grew} grew, ${d.counts.new} new, ${d.counts.shrank} shrank, ${d.counts.gone} gone`),
    ),
    h("h2", null, "Changed allocation sites"),
    table,
  ]);
}
