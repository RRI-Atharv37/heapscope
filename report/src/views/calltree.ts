// Call tree: callers on the outside, allocation sites at the leaves. Rows are virtualised, so a
// tree of hundreds of thousands of nodes costs only the rows on screen.

import type { View } from "../main";
import type { Metric } from "../data";
import { clear, h } from "../dom";
import { count, percent } from "../format";
import { formatMetric, metricSelect } from "./common";

const ROW = 24;
const OVERSCAN = 6;
/** Browsers cap an element at about 33 million px; stay well under it (1M rows x 24 px). */
const MAX_EXPANDED = 1_000_000;

type SortKey = "total" | "self" | "name";

export const callTreeView: View = (host, ctx) => {
  const tree = ctx.tree;
  if (tree.size <= 1) {
    host.append(h("p", { class: "empty" }, "No allocations were recorded."));
    return;
  }

  const expanded = new Set<number>();
  /** While searching, nodes whose automatic state the user flipped. */
  const flipped = new Set<number>();
  let sortKey: SortKey = "total";
  let sortDesc = true;
  let query = "";
  let selected = -1;
  /** Visible rows: node ids in display order. */
  let rows: number[] = [];
  let below: Uint8Array | null = null; // search: has a matching descendant
  let sub: Uint8Array | null = null; // search: matches or has a matching descendant
  let direct: Uint8Array | null = null; // search: matches itself
  let needle = "";

  const metric = (): Metric => ctx.state.metric;

  const isExpanded = (n: number): boolean =>
    query ? (below![n] === 1) !== flipped.has(n) : expanded.has(n);

  const hasChildren = (n: number): boolean => tree.children[n]!.length > 0 && (!query || sub![n] === 1);

  function compare(): (a: number, b: number) => number {
    const dir = sortDesc ? -1 : 1;
    if (sortKey === "name") return (a, b) => dir * tree.name(a).localeCompare(tree.name(b)) || a - b;
    const v = sortKey === "total" ? tree.incl[metric()] : tree.own[metric()];
    return (a, b) => dir * (v[a]! - v[b]!) || a - b;
  }

  function search(): void {
    needle = query.trim().toLowerCase();
    query = needle;
    flipped.clear();
    if (!needle) {
      below = sub = direct = null;
      return;
    }
    const n = tree.size;
    direct = new Uint8Array(n);
    sub = new Uint8Array(n);
    below = new Uint8Array(n);
    for (let i = 1; i < n; i++) {
      const text = `${tree.name(i)} ${tree.module(i) ?? ""} ${tree.file(i) ?? ""}`.toLowerCase();
      if (text.includes(needle)) direct[i] = sub[i] = 1;
    }
    for (let i = n - 1; i >= 1; i--) {
      if (sub[i]) {
        const p = tree.parent[i]!;
        if (p !== 0) {
          below[p] = 1;
          sub[p] = 1;
        }
      }
    }
  }

  function rebuild(): void {
    const cmp = compare();
    const out: number[] = [];
    const walk = (n: number) => {
      const kids = tree.children[n]!;
      const list = kids.length > 1 ? [...kids].sort(cmp) : kids;
      for (const c of list) {
        if (query && sub![c] !== 1) continue;
        out.push(c);
        if (isExpanded(c)) walk(c);
      }
    };
    walk(0);
    rows = out;
    if (selected >= 0 && !rows.includes(selected)) selected = -1;
    sizer.style.height = `${rows.length * ROW}px`;
    status.textContent = query
      ? `${count(rows.length)} rows match "${query}"`
      : `${count(tree.size - 1)} call tree nodes`;
    paint();
  }

  /** Open the heaviest call path so the first screen shows something useful. */
  function openHotPath(): void {
    expanded.clear();
    const kids = tree.childrenBy(metric());
    let n = 0;
    for (let d = 0; d < 8; d++) {
      const top = kids[n]![0];
      if (top === undefined || tree.incl[metric()][top]! <= 0) break;
      if (d > 0) expanded.add(n);
      n = top;
    }
    expanded.add(n);
  }

  function toggle(n: number): void {
    if (!hasChildren(n)) return;
    if (query) {
      if (flipped.has(n)) flipped.delete(n);
      else flipped.add(n);
    } else if (expanded.has(n)) expanded.delete(n);
    else expanded.add(n);
    rebuild();
  }

  function expandAll(): void {
    if (query) {
      flipped.clear();
    } else {
      for (let i = 1; i < Math.min(tree.size, MAX_EXPANDED); i++) if (tree.children[i]!.length) expanded.add(i);
    }
    rebuild();
  }

  // ---- elements ----
  const sizer = h("div", { class: "tree-sizer" });
  const scroller = h("div", { class: "tree-scroll", tabindex: "0", role: "tree", "aria-label": "Call tree" }, sizer);
  const status = h("div", { class: "tree-status", "aria-live": "polite" });

  const searchBox = h("input", {
    type: "search",
    placeholder: "Function, module or file",
    "aria-label": "Search the call tree",
    oninput: () => {
      query = searchBox.value;
      search();
      rebuild();
      scroller.scrollTop = 0;
    },
  });

  const headCell = (key: SortKey, label: string, num: boolean) => {
    const b = h("button", {
      class: num ? "num" : null,
      "aria-sort": sortKey === key ? (sortDesc ? "descending" : "ascending") : null,
      onclick: () => {
        if (sortKey === key) sortDesc = !sortDesc;
        else {
          sortKey = key;
          sortDesc = key !== "name";
        }
        drawHead();
        rebuild();
      },
    });
    b.textContent = label + (sortKey === key ? (sortDesc ? " ▾" : " ▴") : "");
    return b;
  };
  const head = h("div", { class: "tree-head", role: "presentation" });
  function drawHead(): void {
    clear(head);
    const other = metric() === "allocs" ? "Bytes" : "Allocs";
    head.append(
      headCell("name", "Call path", false),
      headCell("total", "Total", true),
      headCell("self", "Self", true),
      h("div", { class: "num" }, "%"),
      h("div", { class: "num" }, other),
    );
  }

  function highlight(text: string): (Node | string)[] {
    if (!needle) return [text];
    const at = text.toLowerCase().indexOf(needle);
    if (at < 0) return [text];
    return [text.slice(0, at), h("mark", null, text.slice(at, at + needle.length)), text.slice(at + needle.length)];
  }

  function rowEl(index: number): HTMLElement {
    const n = rows[index]!;
    const m = metric();
    const total = tree.incl[m][n]!;
    const root = tree.incl[m][0]!;
    const open = hasChildren(n) ? isExpanded(n) : null;
    const other = tree.incl[m === "allocs" ? "alloc_bytes" : "allocs"][n]!;
    const loc = tree.location(n);
    const depth = depthOf(n);
    return h(
      "div",
      {
        class: "tree-row",
        role: "treeitem",
        "aria-level": depth + 1,
        "aria-expanded": open === null ? null : String(open),
        "aria-selected": n === selected ? "true" : "false",
        "data-node": n,
        style: `top:${index * ROW}px`,
        title: [tree.name(n), tree.module(n), loc].filter(Boolean).join("\n"),
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
        h("span", { class: "text" }, ...highlight(tree.name(n))),
        tree.module(n) ? h("span", { class: "mod" }, tree.module(n)) : null,
      ),
      h("div", { class: "num" }, formatMetric(m, total)),
      h("div", { class: "num" }, formatMetric(m, tree.own[m][n]!)),
      h("div", { class: "num pct" }, h("i", { style: `width:${root > 0 ? Math.max(0, Math.min(100, (total / root) * 100)) : 0}%` }), h("span", null, percent(total, root))),
      h("div", { class: "num" }, formatMetric(m === "allocs" ? "alloc_bytes" : "allocs", other)),
    );
  }

  const depthCache = new Map<number, number>();
  function depthOf(n: number): number {
    let d = depthCache.get(n);
    if (d === undefined) {
      d = n === 0 ? -1 : depthOf(tree.parent[n]!) + 1;
      depthCache.set(n, d);
    }
    return d;
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
          if (!isExpanded(n)) toggle(n);
          else select(at + 1);
        }
        break;
      case "ArrowLeft":
        if (n >= 0) {
          if (hasChildren(n) && isExpanded(n)) toggle(n);
          else if (tree.parent[n] !== 0) select(rows.indexOf(tree.parent[n]!));
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

  function resetView(): void {
    openHotPath();
    drawHead();
    rebuild();
  }

  host.append(
    h(
      "div",
      { class: "toolbar" },
      h("label", null, "Search", searchBox),
      metricSelect(ctx, resetView),
      h("button", { onclick: expandAll }, "Expand all"),
      h("button", { onclick: () => { expanded.clear(); flipped.clear(); rebuild(); } }, "Collapse all"),
      h("button", { onclick: () => { searchBox.value = ""; query = ""; search(); resetView(); } }, "Reset"),
    ),
    head,
    scroller,
    status,
  );
  resetView();
  return () => window.removeEventListener("resize", paint);
};
