import { Tree, load, type Metric, type ReportData } from "./data";
import { clear, h } from "./dom";
import { banners } from "./banner";
import { summaryView } from "./views/summary";
import { callTreeView } from "./views/calltree";
import { flameGraphView } from "./views/flamegraph";

export interface Ctx {
  data: ReportData;
  tree: Tree;
  /** Shared by the views that show a metric, so switching tabs keeps the choice. */
  state: { metric: Metric };
}

/** Called with the element to fill; returns a cleanup function if it needs one. */
export type View = (host: HTMLElement, ctx: Ctx) => (() => void) | void;

const VIEWS: { id: string; label: string; render: View }[] = [
  { id: "summary", label: "Summary", render: summaryView },
  { id: "calltree", label: "Call tree", render: callTreeView },
  { id: "flame", label: "Flame graph", render: flameGraphView },
];

function currentView(): string {
  const id = location.hash.replace(/^#\/?/, "");
  return VIEWS.some((v) => v.id === id) ? id : "summary";
}

function start(app: HTMLElement): void {
  const loaded = load();
  if ("error" in loaded) {
    app.append(h("main", null, h("p", { class: "empty" }, loaded.error)));
    return;
  }
  const { data } = loaded;
  const ctx: Ctx = { data, tree: new Tree(data.tree), state: { metric: "alloc_bytes" } };
  const cap = data.summary.capture;
  document.title = `HeapScope: ${cap.exe.split(/[\\/]/).pop() || "report"}`;

  const tabs = h("nav", { class: "tabs", "aria-label": "Views" });
  const main = h("main", { id: "view" });
  app.append(
    h(
      "header",
      { class: "top" },
      h("h1", null, "HeapScope"),
      h("span", { class: "prog" }, cap.command_line || cap.exe),
      tabs,
    ),
    banners(cap),
    main,
  );

  let cleanup: (() => void) | void;
  const show = () => {
    const id = currentView();
    cleanup?.();
    clear(tabs);
    for (const v of VIEWS) {
      tabs.append(h("a", { href: `#/${v.id}`, "aria-current": v.id === id ? "page" : null }, v.label));
    }
    clear(main);
    cleanup = VIEWS.find((v) => v.id === id)!.render(main, ctx);
  };
  window.addEventListener("hashchange", show);
  show();
}

start(document.getElementById("app")!);
