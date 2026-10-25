// The capture-mode banner (prd R-8): shown above every view, because a number read without
// knowing the capture was sampled, attached late or lossy is a wrong number.

import type { CaptureInfo, DiffInfo } from "./data";
import { h } from "./dom";
import { count } from "./format";

export interface BannerMessage {
  kind: "attached" | "sampled" | "filtered" | "lost" | "unclean";
  title: string;
  text: string;
}

export function bannerMessages(cap: CaptureInfo): BannerMessage[] {
  const out: BannerMessage[] = [];
  if (cap.attached) {
    out.push({
      kind: "attached",
      title: "Attached late.",
      text: "Allocations made before the attach are unknown, so these are net changes since attach, not totals, and live memory is growth rather than leaks.",
    });
  }
  if (cap.mode === "sampled") {
    out.push({
      kind: "sampled",
      title: `Sampled 1 in ${cap.sample_rate}.`,
      text: `Counts and sizes are estimates, scaled up by ${cap.sample_rate}. Small sites may be missing.`,
    });
  }
  if (cap.min_lifetime_ms > 0) {
    out.push({
      kind: "filtered",
      title: "Short-lived allocations filtered.",
      text: `Allocations freed within ${cap.min_lifetime_ms} ms were folded into per-site totals and not traced one by one, so the peak is slightly understated.`,
    });
  }
  if (cap.dropped.overflow > 0) {
    out.push({
      kind: "lost",
      title: "Events were lost.",
      text: `${count(cap.dropped.overflow)} events did not fit the capture buffers, so every figure is a lower bound.`,
    });
  }
  if (!cap.complete) {
    out.push({
      kind: "unclean",
      title: "Capture did not finish cleanly.",
      text: "The program was killed or crashed; the end of the trace is where the file stops, not where the program exited.",
    });
  }
  return out;
}

export function banners(cap: CaptureInfo): HTMLElement {
  const box = h("div", { class: "banners", role: "region", "aria-label": "Capture mode" });
  for (const b of bannerMessages(cap)) {
    box.append(h("p", { class: "banner" }, h("strong", null, b.title), b.text));
  }
  return box;
}

/** The banners of a diff report: the comparison's own notes, and what else is wrong with either side. */
export function diffBanners(d: DiffInfo): HTMLElement {
  const box = h("div", { class: "banners", role: "region", "aria-label": "Capture mode" });
  for (const n of d.notes) {
    box.append(h("p", { class: "banner" }, n.charAt(0).toUpperCase() + n.slice(1) + "."));
  }
  // Attached, sampled and unclean captures are already in the notes.
  for (const [name, side] of [["Before", d.before], ["After", d.after]] as const) {
    for (const m of bannerMessages(side.capture)) {
      if (m.kind === "filtered" || m.kind === "lost") {
        box.append(h("p", { class: "banner" }, h("strong", null, `${name}: ${m.title}`), m.text));
      }
    }
  }
  return box;
}
