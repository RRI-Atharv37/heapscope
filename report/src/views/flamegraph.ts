// Flame graph on a canvas: one box per call-tree node, width proportional to the chosen metric,
// callers on top and allocation sites below. Click a box to zoom into it; boxes under a pixel
// wide are not drawn, so even a very large tree costs a bounded amount of work.

import type { View } from "../main";
import { append, clear, h } from "../dom";
import { percent } from "../format";
import { formatMetric, metricSelect } from "./common";

const ROW = 18;
const MIN_PX = 1;
const FONT = '11px system-ui, -apple-system, "Segoe UI", sans-serif';

interface Box {
  node: number;
  x: number;
  w: number;
}

function hue(name: string): number {
  let hash = 2166136261;
  for (let i = 0; i < name.length; i++) hash = Math.imul(hash ^ name.charCodeAt(i), 16777619);
  return (hash >>> 0) % 360;
}

export const flameGraphView: View = (host, ctx) => {
  const tree = ctx.tree;
  if (tree.size <= 1) {
    host.append(h("p", { class: "empty" }, "No allocations were recorded."));
    return;
  }

  let zoom = 0;
  /** Drawn boxes per row, left to right, for hit testing. */
  let layout: Box[][] = [];
  let hover: { row: number; box: Box } | null = null;

  const canvas = h("canvas", {
    role: "img",
    "aria-label": "Flame graph of allocations. The Call tree tab has the same data as a keyboard-accessible table.",
  });
  const overlay = h("canvas", { "aria-hidden": "true" });
  overlay.style.cssText = "position:absolute;left:0;top:0;pointer-events:none";
  const wrap = h("div", { class: "flame-wrap" }, canvas, overlay);
  const crumbs = h("div", { class: "crumbs" });
  const tip = h("div", { class: "flame-tip", hidden: true });
  const empty = h("p", { class: "empty", hidden: true }, "Nothing to show for this metric.");

  const metric = () => ctx.state.metric;
  const darkMode = () => window.matchMedia("(prefers-color-scheme: dark)").matches;

  function draw(): void {
    const m = metric();
    const total = tree.incl[m][zoom]!;
    empty.hidden = total > 0;
    wrap.hidden = total <= 0;
    drawCrumbs();
    if (total <= 0) return;

    const width = Math.max(200, Math.floor(wrap.clientWidth));
    const dpr = window.devicePixelRatio || 1;
    const kids = tree.childrenBy(m);
    const inclusive = tree.incl[m];
    layout = [];

    // Depth-first from the zoom root; every box's children split its width by their share.
    const stack: { node: number; x: number; w: number; row: number }[] = [{ node: zoom, x: 0, w: width, row: 0 }];
    while (stack.length) {
      const { node, x, w, row } = stack.pop()!;
      (layout[row] ??= []).push({ node, x, w });
      const nodeTotal = inclusive[node]!;
      let cx = x;
      const pending: { node: number; x: number; w: number; row: number }[] = [];
      for (const c of kids[node]!) {
        const cw = (inclusive[c]! / nodeTotal) * w;
        if (cw < MIN_PX) break; // sorted largest first, so the rest are smaller still
        pending.push({ node: c, x: cx, w: cw, row: row + 1 });
        cx += cw;
      }
      // Pushed in reverse so rows fill left to right.
      for (let i = pending.length - 1; i >= 0; i--) stack.push(pending[i]!);
    }
    // The stack visits a subtree fully before its next sibling, so each row is already in x order.

    const height = layout.length * ROW;
    for (const c of [canvas, overlay]) {
      c.width = Math.floor(width * dpr);
      c.height = Math.floor(height * dpr);
      c.style.width = `${width}px`;
      c.style.height = `${height}px`;
    }
    const g = canvas.getContext("2d")!;
    g.scale(dpr, dpr);
    g.font = FONT;
    g.textBaseline = "middle";
    const dark = darkMode();
    const textColor = getComputedStyle(document.documentElement).getPropertyValue("--flame-text").trim() || "#111";
    layout.forEach((boxes, row) => {
      for (const b of boxes) {
        g.fillStyle = dark ? `hsl(${hue(tree.name(b.node))} 38% 30%)` : `hsl(${hue(tree.name(b.node))} 62% 74%)`;
        g.fillRect(b.x, row * ROW, Math.max(b.w - 1, 0.5), ROW - 1);
        if (b.w > 36) {
          g.fillStyle = textColor;
          g.fillText(fit(g, node_label(b.node), b.w - 6), b.x + 3, row * ROW + ROW / 2);
        }
      }
    });
    drawHover();
  }

  const node_label = (n: number): string => (n === zoom && zoom === 0 ? "all allocations" : tree.name(n));

  function fit(g: CanvasRenderingContext2D, text: string, room: number): string {
    const full = g.measureText(text).width;
    if (full <= room) return text;
    const chars = Math.floor((text.length * room) / full) - 1;
    return chars > 0 ? text.slice(0, chars) + "…" : "";
  }

  function drawHover(): void {
    const g = overlay.getContext("2d")!;
    g.setTransform(1, 0, 0, 1, 0, 0);
    g.clearRect(0, 0, overlay.width, overlay.height);
    if (!hover) return;
    const dpr = window.devicePixelRatio || 1;
    g.scale(dpr, dpr);
    g.lineWidth = 2;
    g.strokeStyle = getComputedStyle(document.documentElement).getPropertyValue("--focus").trim() || "#0a58ad";
    g.strokeRect(hover.box.x + 1, hover.row * ROW + 1, Math.max(hover.box.w - 3, 1), ROW - 3);
  }

  function drawCrumbs(): void {
    clear(crumbs);
    const path = tree.ancestry(zoom);
    path.forEach((n, i) => {
      if (i > 0) crumbs.append(h("span", { "aria-hidden": "true" }, "›"));
      const label = n === 0 ? "all allocations" : tree.name(n);
      crumbs.append(
        i === path.length - 1
          ? h("strong", null, label)
          : h("button", { title: label, onclick: () => { zoom = n; draw(); } }, label.length > 40 ? label.slice(0, 39) + "…" : label),
      );
    });
  }

  function hit(e: MouseEvent): { row: number; box: Box } | null {
    const r = canvas.getBoundingClientRect();
    const x = e.clientX - r.left;
    const row = Math.floor((e.clientY - r.top) / ROW);
    const boxes = layout[row];
    if (!boxes) return null;
    let lo = 0;
    let hi = boxes.length - 1;
    while (lo <= hi) {
      const mid = (lo + hi) >> 1;
      const b = boxes[mid]!;
      if (x < b.x) hi = mid - 1;
      else if (x >= b.x + b.w) lo = mid + 1;
      else return { row, box: b };
    }
    return null;
  }

  function showTip(e: MouseEvent, found: { row: number; box: Box }): void {
    const n = found.box.node;
    const m = metric();
    const total = tree.incl[m][n]!;
    const loc = tree.location(n);
    clear(tip);
    append(tip, [
      h("div", { class: "fn" }, n === 0 ? "all allocations" : tree.name(n)),
      tree.module(n) ? h("div", { class: "muted" }, tree.module(n)) : null,
      loc ? h("div", { class: "muted" }, loc) : null,
      h(
        "div",
        null,
        `${formatMetric(m, total)} total (${percent(total, tree.incl[m][zoom]!)} of view, ${percent(total, tree.incl[m][0]!)} of all)`,
      ),
      h("div", null, `${formatMetric(m, tree.own[m][n]!)} self`),
      n !== zoom ? h("div", { class: "muted" }, "Click to zoom in") : null,
    ]);
    tip.hidden = false;
    const pad = 14;
    const w = tip.offsetWidth;
    const hgt = tip.offsetHeight;
    tip.style.left = `${Math.min(e.clientX + pad, window.innerWidth - w - 8)}px`;
    tip.style.top = `${e.clientY + pad + hgt > window.innerHeight ? e.clientY - hgt - pad : e.clientY + pad}px`;
  }

  canvas.addEventListener("mousemove", (e) => {
    const found = hit(e);
    if (found?.box.node !== hover?.box.node || found?.row !== hover?.row) {
      hover = found;
      drawHover();
    }
    if (found) showTip(e, found);
    else tip.hidden = true;
  });
  canvas.addEventListener("mouseleave", () => {
    hover = null;
    tip.hidden = true;
    drawHover();
  });
  canvas.addEventListener("click", (e) => {
    const found = hit(e);
    if (found && found.box.node !== zoom) {
      zoom = found.box.node;
      hover = null;
      tip.hidden = true;
      draw();
    }
  });

  const observer = new ResizeObserver(() => draw());
  const toolbar = h(
    "div",
    { class: "toolbar" },
    metricSelect(ctx, () => {
      draw();
    }),
    h("button", { onclick: () => { zoom = 0; draw(); } }, "Reset zoom"),
    h("span", { class: "spacer" }),
    h("span", { class: "muted" }, "Width is proportional to the figure; click a box to zoom."),
  );
  append(host, [toolbar, crumbs, h("div", { style: "height:8px" }), wrap, empty, tip]);
  draw();
  observer.observe(wrap);
  const onScheme = () => draw();
  const media = window.matchMedia("(prefers-color-scheme: dark)");
  media.addEventListener("change", onScheme);
  return () => {
    observer.disconnect();
    media.removeEventListener("change", onScheme);
    tip.remove();
  };
};
