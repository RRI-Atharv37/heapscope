import { Shape, Tree, load, type CaptureData, type DiffData, type Metric } from "./data";
import { clear, h } from "./dom";
import { banners, diffBanners } from "./banner";
import { summaryView } from "./views/summary";
import { callTreeView } from "./views/calltree";
import { flameGraphView } from "./views/flamegraph";
import { timelineView } from "./views/timeline";
import { DeltaTree, deltaTreeView, diffSummary, diffView } from "./views/diff";

export interface Ctx {
  data: CaptureData;
  tree: Tree;
  /** Shared by the views, so switching tabs keeps the choices. */
  state: {
    metric: Metric;
    /** Id of the moment picked on the timeline. */
    moment?: string;
    /** The pair of moments the diff view compares. */
    diff?: { from: string; to: string };
  };
}

/** Called with the element to fill; returns a cleanup function if it needs one. */
export type View = (host: HTMLElement, ctx: Ctx) => (() => void) | void;

interface Tab {
  id: string;
  label: string;
  render: (host: HTMLElement) => (() => void) | void;
}

function captureTabs(data: CaptureData): { tabs: Tab[]; ctx: Ctx } {
  const ctx: Ctx = { data, tree: new Tree(data.tree), state: { metric: "alloc_bytes" } };
  const tab = (id: string, label: string, view: View): Tab => ({ id, label, render: (host) => view(host, ctx) });
  return {
    ctx,
    tabs: [
      tab("summary", "Summary", summaryView),
      tab("calltree", "Call tree", callTreeView),
      tab("flame", "Flame graph", flameGraphView),
      tab("timeline", "Timeline", timelineView),
      tab("diff", "Diff", diffView),
    ],
  };
}

function diffTabs(data: DiffData): Tab[] {
  const d = data.diff;
  const delta = new DeltaTree(
    new Shape(d.tree),
    { allocs: Float64Array.from(d.tree.before_allocs), bytes: Float64Array.from(d.tree.before_bytes) },
    { allocs: Float64Array.from(d.tree.after_allocs), bytes: Float64Array.from(d.tree.after_bytes) },
  );
  return [
    { id: "summary", label: "Summary", render: (host) => diffSummary(host, d) },
    { id: "delta", label: "Delta tree", render: (host) => deltaTreeView(host, delta) },
  ];
}

function start(app: HTMLElement): void {
  const loaded = load();
  if ("error" in loaded) {
    app.append(h("main", null, h("p", { class: "empty" }, loaded.error)));
    return;
  }
  const { data } = loaded;

  let tabs: Tab[];
  let title: string;
  let subtitle: string;
  let banner: HTMLElement;
  if ("diff" in data) {
    const d = data.diff;
    tabs = diffTabs(data);
    title = `HeapScope diff: ${d.before.label} vs ${d.after.label}`;
    subtitle = `${d.before.label} (${d.before.at}) → ${d.after.label} (${d.after.at})`;
    banner = diffBanners(d);
  } else {
    const built = captureTabs(data);
    tabs = built.tabs;
    const cap = data.summary.capture;
    title = `HeapScope: ${cap.exe.split(/[\\/]/).pop() || "report"}`;
    subtitle = cap.command_line || cap.exe;
    banner = banners(cap);
  }
  document.title = title;

  const nav = h("nav", { class: "tabs", "aria-label": "Views" });
  const main = h("main", { id: "view" });
  app.append(
    h("header", { class: "top" }, h("h1", null, "HeapScope"), h("span", { class: "prog" }, subtitle), nav),
    banner,
    main,
  );

  const currentView = (): string => {
    const id = location.hash.replace(/^#\/?/, "");
    return tabs.some((t) => t.id === id) ? id : tabs[0]!.id;
  };

  let cleanup: (() => void) | void;
  const show = () => {
    const id = currentView();
    cleanup?.();
    clear(nav);
    for (const t of tabs) {
      nav.append(h("a", { href: `#/${t.id}`, "aria-current": t.id === id ? "page" : null }, t.label));
    }
    clear(main);
    cleanup = tabs.find((t) => t.id === id)!.render(main);
  };
  window.addEventListener("hashchange", show);
  show();
}

start(document.getElementById("app")!);
