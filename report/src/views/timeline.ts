// Live memory over time. Markers, the peak and the end of the trace are the moments whose live
// set the report keeps: click one (or pick it from the list) to see which call paths held the
// memory then, and open the diff view between two of them.

import type { View } from "../main";
import { liveAt, moments, type Moment } from "../data";
import { append, clear, h } from "../dom";
import { bytes, count, duration, percent } from "../format";

const HEIGHT = 300;
const PAD = { l: 72, r: 20, t: 40, b: 30 };
const FONT = '11px system-ui, -apple-system, "Segoe UI", sans-serif';
/** How close (px) a click must be to a moment to pick it. */
const REACH = 14;
const FIRST_ROWS = 25;
const MORE_ROWS = 100;

type Unit = "bytes" | "allocs";

/** A round step of about `raw`: 1, 2 or 5 times a power of ten. */
function niceStep(raw: number): number {
  const mag = Math.pow(10, Math.floor(Math.log10(raw)));
  return ([1, 2, 5, 10].find((s) => s * mag >= raw) ?? 10) * mag;
}

/** Round axis values from 0 up to `max`; byte axes step in whole KiB/MiB/GiB units. */
function ticks(max: number, unit: Unit, wanted: number): number[] {
  let scale = 1;
  if (unit === "bytes") while (max / scale >= 1024 * wanted && scale < 1024 ** 4) scale *= 1024;
  let step = niceStep(max / scale / wanted) * scale;
  if (unit === "allocs" || scale === 1) step = Math.max(1, step);
  const out: number[] = [];
  for (let v = 0; v <= max; v += step) out.push(v);
  return out;
}

const css = (name: string, fallback: string): string =>
  getComputedStyle(document.documentElement).getPropertyValue(name).trim() || fallback;

export const timelineView: View = (host, ctx) => {
  const { data, tree } = ctx;
  const points = data.timeline;
  if (points.length === 0) {
    host.append(h("p", { class: "empty" }, "No allocations were recorded."));
    return;
  }
  const list = moments(data);
  let unit: Unit = "bytes";
  let selected: Moment = list.find((m) => m.id === ctx.state.moment) ?? list.find((m) => m.kind === "peak") ?? list[list.length - 1]!;
  let hover = -1;
  let shown = FIRST_ROWS;

  const xMax = Math.max(1, points[points.length - 1]![0], ...list.map((m) => m.ts_ns));
  const pointValue = (i: number): number => points[i]![unit === "bytes" ? 1 : 2];
  const momentValue = (m: Moment): number => (unit === "bytes" ? m.live_bytes : m.live_allocs);
  const format = (v: number): string => (unit === "bytes" ? bytes(v) : count(v));

  const canvas = h("canvas", {
    role: "img",
    "aria-label":
      "Line chart of live memory over time, with markers and the peak. The list of moments below it is the keyboard-accessible version.",
  });
  const wrap = h("div", { class: "flame-wrap timeline-wrap" }, canvas);
  const tip = h("div", { class: "flame-tip", hidden: true });
  const chips = h("div", { class: "chips", role: "group", "aria-label": "Moments" });
  const detail = h("section", { "aria-live": "polite" });

  // Plot geometry of the last draw, for hit testing.
  let width = 0;
  const plotW = (): number => Math.max(1, width - PAD.l - PAD.r);
  const px = (ts: number): number => PAD.l + (ts / xMax) * plotW();
  let yMax = 1;
  const py = (v: number): number => HEIGHT - PAD.b - (v / yMax) * (HEIGHT - PAD.t - PAD.b);

  function draw(): void {
    width = Math.max(320, Math.floor(wrap.clientWidth));
    const dpr = window.devicePixelRatio || 1;
    canvas.width = Math.floor(width * dpr);
    canvas.height = Math.floor(HEIGHT * dpr);
    canvas.style.width = `${width}px`;
    canvas.style.height = `${HEIGHT}px`;
    const g = canvas.getContext("2d")!;
    g.setTransform(dpr, 0, 0, dpr, 0, 0);
    g.clearRect(0, 0, width, HEIGHT);
    g.font = FONT;

    const muted = css("--fg-muted", "#4b5560");
    const border = css("--border", "#c9d1d9");
    const accent = css("--accent", "#0a58ad");
    const area = css("--bar", "#6aa4e0");
    const bad = css("--bad", "#a4252a");
    const bg = css("--bg", "#fff");

    const top = Math.max(...points.map((_, i) => pointValue(i)), ...list.map(momentValue), 1);
    const yTicks = ticks(top * 1.05, unit, 4);
    yMax = Math.max(top * 1.05, yTicks[yTicks.length - 1]!);

    // Grid and axes.
    g.textBaseline = "middle";
    g.textAlign = "right";
    g.lineWidth = 1;
    for (const t of yTicks) {
      const y = Math.round(py(t)) + 0.5;
      g.strokeStyle = border;
      g.beginPath();
      g.moveTo(PAD.l, y);
      g.lineTo(width - PAD.r, y);
      g.stroke();
      g.fillStyle = muted;
      g.fillText(t === 0 ? "0" : format(t), PAD.l - 8, y);
    }
    g.textAlign = "center";
    g.textBaseline = "top";
    const xStep = niceStep(xMax / 6);
    for (let t = 0; t <= xMax; t += xStep) {
      g.fillStyle = muted;
      g.fillText(t === 0 ? "0" : duration(t), Math.min(px(t), width - PAD.r - 14), HEIGHT - PAD.b + 8);
    }

    // The curve.
    const base = py(0);
    g.beginPath();
    g.moveTo(px(points[0]![0]), base);
    points.forEach((p, i) => g.lineTo(px(p[0]), py(pointValue(i))));
    g.lineTo(px(points[points.length - 1]![0]), base);
    g.closePath();
    g.globalAlpha = 0.35;
    g.fillStyle = area;
    g.fill();
    g.globalAlpha = 1;
    g.beginPath();
    points.forEach((p, i) => (i === 0 ? g.moveTo(px(p[0]), py(pointValue(i))) : g.lineTo(px(p[0]), py(pointValue(i)))));
    g.lineWidth = 1.5;
    g.strokeStyle = accent;
    g.stroke();

    // Moments: markers as dashed lines with a flag, the peak as a diamond, the end as a dot.
    const rowEnd = [-Infinity, -Infinity];
    g.textBaseline = "alphabetic";
    for (const m of list) {
      const x = px(m.ts_ns);
      const y = py(momentValue(m));
      if (m.kind === "marker") {
        g.setLineDash([4, 3]);
        g.lineWidth = 1;
        g.strokeStyle = muted;
        g.beginPath();
        g.moveTo(x, PAD.t - 4);
        g.lineTo(x, base);
        g.stroke();
        g.setLineDash([]);
        const label = m.label.length > 18 ? m.label.slice(0, 17) + "…" : m.label;
        const w = g.measureText(label).width;
        const right = x + 4 + w > width - 4;
        const left = right ? x - 4 - w : x + 4;
        const row = rowEnd.findIndex((end) => left >= end);
        if (row >= 0) {
          g.textAlign = "left";
          g.fillStyle = muted;
          g.fillText(label, left, PAD.t - 8 - (1 - row) * 13 + 0);
          rowEnd[row] = left + w + 8;
        }
      } else if (m.kind === "peak") {
        g.fillStyle = bad;
        g.strokeStyle = bg;
        g.lineWidth = 1.5;
        g.beginPath();
        g.moveTo(x, y - 7);
        g.lineTo(x + 7, y);
        g.lineTo(x, y + 7);
        g.lineTo(x - 7, y);
        g.closePath();
        g.fill();
        g.stroke();
      } else {
        g.fillStyle = accent;
        g.strokeStyle = bg;
        g.lineWidth = 1.5;
        g.beginPath();
        g.arc(x, y, 5, 0, Math.PI * 2);
        g.fill();
        g.stroke();
      }
    }

    // The selected moment.
    {
      const x = px(selected.ts_ns);
      g.strokeStyle = accent;
      g.lineWidth = 2;
      g.beginPath();
      g.moveTo(x, PAD.t - 4);
      g.lineTo(x, base);
      g.stroke();
      g.beginPath();
      g.arc(x, py(momentValue(selected)), 10, 0, Math.PI * 2);
      g.stroke();
    }

    // The point under the pointer.
    if (hover >= 0) {
      const x = px(points[hover]![0]);
      g.strokeStyle = muted;
      g.lineWidth = 1;
      g.beginPath();
      g.moveTo(x + 0.5, PAD.t);
      g.lineTo(x + 0.5, base);
      g.stroke();
      g.fillStyle = accent;
      g.beginPath();
      g.arc(x, py(pointValue(hover)), 3.5, 0, Math.PI * 2);
      g.fill();
    }
  }

  const localX = (e: MouseEvent): number => e.clientX - canvas.getBoundingClientRect().left;

  function nearestPoint(x: number): number {
    const ts = ((x - PAD.l) / plotW()) * xMax;
    let lo = 0;
    let hi = points.length - 1;
    while (lo < hi) {
      const mid = (lo + hi) >> 1;
      if (points[mid]![0] < ts) lo = mid + 1;
      else hi = mid;
    }
    return lo > 0 && Math.abs(points[lo - 1]![0] - ts) < Math.abs(points[lo]![0] - ts) ? lo - 1 : lo;
  }

  function nearestMoment(x: number): Moment | null {
    let best: Moment | null = null;
    let dist = REACH;
    for (const m of list) {
      const d = Math.abs(px(m.ts_ns) - x);
      if (d <= dist) {
        best = m;
        dist = d;
      }
    }
    return best;
  }

  canvas.addEventListener("mousemove", (e) => {
    const x = localX(e);
    if (x < PAD.l - 4 || x > width - PAD.r + 4) {
      leave();
      return;
    }
    const i = nearestPoint(x);
    const m = nearestMoment(x);
    if (i !== hover) {
      hover = i;
      draw();
    }
    const p = points[i]!;
    clear(tip);
    append(tip, [
      m ? h("div", { class: "fn" }, m.kind === "marker" ? `Marker: ${m.label}` : m.kind === "peak" ? "Peak" : "End of trace") : null,
      h("div", null, `${duration(p[0])}: ${bytes(p[1])} live, ${count(p[2])} allocations`),
      m ? h("div", { class: "muted" }, "Click to show what was live") : null,
    ]);
    tip.hidden = false;
    const pad = 14;
    const w = tip.offsetWidth;
    const hgt = tip.offsetHeight;
    tip.style.left = `${Math.min(e.clientX + pad, window.innerWidth - w - 8)}px`;
    tip.style.top = `${e.clientY + pad + hgt > window.innerHeight ? e.clientY - hgt - pad : e.clientY + pad}px`;
    canvas.style.cursor = m ? "pointer" : "default";
  });
  function leave(): void {
    tip.hidden = true;
    if (hover >= 0) {
      hover = -1;
      draw();
    }
  }
  canvas.addEventListener("mouseleave", leave);
  canvas.addEventListener("click", (e) => {
    const m = nearestMoment(localX(e));
    if (m) select(m);
  });

  function select(m: Moment): void {
    selected = m;
    ctx.state.moment = m.id;
    shown = FIRST_ROWS;
    drawChips();
    drawDetail();
    draw();
  }

  const when = (m: Moment): string => `at ${duration(m.ts_ns)}`;

  function drawChips(): void {
    clear(chips);
    for (const m of list) {
      chips.append(
        h(
          "button",
          { class: "chip", "aria-pressed": String(m.id === selected.id), onclick: () => select(m) },
          h("strong", null, m.label),
          h("span", { class: "muted" }, ` ${when(m)} · ${bytes(m.live_bytes)}`),
        ),
      );
    }
  }

  function drawDetail(): void {
    clear(detail);
    const m = selected;
    const live = liveAt(data, tree, m);
    const rows: number[] = [];
    for (let i = 1; i < tree.size; i++) if (live.bytes[i]! > 0 || live.allocs[i]! > 0) rows.push(i);
    rows.sort((a, b) => live.bytes[b]! - live.bytes[a]! || live.allocs[b]! - live.allocs[a]! || a - b);
    const peak = data.summary.peak.live_bytes;

    const idx = list.findIndex((x) => x.id === m.id);
    const others = list.filter((x) => x.id !== m.id);
    const defaultOther = list[idx - 1] ?? list[idx + 1];
    const against = h(
      "select",
      { "aria-label": "Compare with" },
      ...others.map((o) => h("option", { value: o.id, selected: o.id === defaultOther?.id }, `${o.label} (${when(o)})`)),
    );
    const compare = () => {
      const other = list.find((x) => x.id === against.value);
      if (!other) return;
      // Earlier moment first, so the change reads as what happened between them.
      const [from, to] = list.indexOf(other) < idx ? [other, m] : [m, other];
      ctx.state.diff = { from: from.id, to: to.id };
      location.hash = "#/diff";
    };

    append(detail, [
      h("h2", null, `Live at ${m.kind === "marker" ? `marker "${m.label}"` : m.label}`),
      h(
        "p",
        null,
        `${when(m).replace("at", "At")}: ${bytes(m.live_bytes)} in ${count(m.live_allocs)} allocations`,
        peak > 0 && m.kind !== "peak" ? `, ${percent(m.live_bytes, peak)} of the peak` : "",
        ".",
      ),
      others.length
        ? h(
            "div",
            { class: "toolbar" },
            h("label", null, "Compare with", against),
            h("button", { onclick: compare }, "Open diff"),
          )
        : null,
      rows.length === 0
        ? h("p", { class: "empty" }, "Nothing was live at this moment.")
        : h(
            "div",
            { class: "scroll-x" },
            h(
              "table",
              { class: "data" },
              h(
                "thead",
                null,
                h("tr", null, h("th", { class: "num" }, "Live"), h("th", { class: "num" }, "Share"), h("th", { class: "num" }, "Allocations"), h("th", null, "Allocation site")),
              ),
              h(
                "tbody",
                null,
                ...rows.slice(0, shown).map((n) => {
                  const callers = tree.ancestry(n).slice(1, -1).reverse().slice(0, 4).map((c) => tree.name(c));
                  const where = tree.location(n) ?? tree.module(n);
                  return h(
                    "tr",
                    null,
                    h("td", { class: "num" }, bytes(live.bytes[n]!)),
                    h("td", { class: "num" }, percent(live.bytes[n]!, m.live_bytes)),
                    h("td", { class: "num" }, count(live.allocs[n]!)),
                    h(
                      "td",
                      { class: "site" },
                      h("code", null, tree.name(n)),
                      where ? h("span", { class: "loc" }, where) : null,
                      callers.length ? h("span", { class: "loc" }, "from " + callers.join(" ← ")) : null,
                    ),
                  );
                }),
              ),
            ),
          ),
      rows.length > shown
        ? h(
            "p",
            { class: "muted" },
            `Showing ${count(shown)} of ${count(rows.length)} sites. `,
            h("button", { onclick: () => { shown += MORE_ROWS; drawDetail(); } }, "Show more"),
          )
        : null,
    ]);
  }

  const toolbar = h(
    "div",
    { class: "toolbar" },
    h(
      "label",
      null,
      "Show",
      h(
        "select",
        {
          onchange: (e: Event) => {
            unit = (e.target as HTMLSelectElement).value as Unit;
            draw();
          },
        },
        h("option", { value: "bytes" }, "Live bytes"),
        h("option", { value: "allocs" }, "Live allocations"),
      ),
    ),
    h("span", { class: "spacer" }),
    h("span", { class: "muted" }, "◆ peak · dashed line = marker · ● end. Click one to see what was live."),
  );

  append(host, [toolbar, wrap, chips, detail, tip]);
  drawChips();
  drawDetail();
  draw();

  const observer = new ResizeObserver(() => draw());
  observer.observe(wrap);
  const media = window.matchMedia("(prefers-color-scheme: dark)");
  media.addEventListener("change", draw);
  return () => {
    observer.disconnect();
    media.removeEventListener("change", draw);
    tip.remove();
  };
};
