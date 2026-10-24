import type { Ctx } from "../main";
import { METRICS, metricInfo, type Metric } from "../data";
import { bytes, count } from "../format";
import { h } from "../dom";

/** The "Show" drop-down shared by the call tree and the flame graph. */
export function metricSelect(ctx: Ctx, onChange: () => void): HTMLElement {
  const select = h(
    "select",
    {
      id: "metric",
      onchange: () => {
        ctx.state.metric = select.value as Metric;
        onChange();
      },
    },
    ...METRICS.map((m) => h("option", { value: m.id, selected: m.id === ctx.state.metric }, m.label)),
  );
  return h("label", null, "Show", select);
}

export function formatMetric(metric: Metric, v: number): string {
  return metricInfo(metric).unit === "bytes" ? bytes(v) : count(v);
}
