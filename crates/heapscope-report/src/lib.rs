//! The HTML report: one self-contained page (no network, no external files) holding the summary,
//! call tree and flame graph of a trace.
//!
//! The page itself is built from `report/` (TypeScript) and committed as
//! `assets/report-template.html`, so a Rust build needs no Node. `render` injects the data of an
//! [`Analysis`] into that template's `hs-data` script element.
//!
//! The embedded data is `report_version` 1: the frozen summary (`docs/json-schema.md`) under
//! `summary`, plus the whole call tree in columns under `tree`. It is private to the page and is
//! allowed to change with the template; scripts should read `heapscope analyze -o` instead.

use std::collections::HashMap;

use heapscope_analysis::json::{self, Json};
use heapscope_analysis::leaks::LeakReport;
use heapscope_analysis::{Analysis, FrameInfo};

/// The page, with `TOKEN` where the data goes.
pub const TEMPLATE: &str = include_str!("../assets/report-template.html");

/// Replaced by the data. The template carries it exactly once, in the `hs-data` script element.
pub const TOKEN: &str = "{{HS_DATA}}";

pub const REPORT_VERSION: u64 = 1;

/// A complete HTML page for `analysis`.
pub fn render(analysis: &Analysis, leaks: &LeakReport) -> String {
    inject(TEMPLATE, &data(analysis, leaks))
}

/// Put `data` into `template` as the content of a script element.
fn inject(template: &str, data: &Json) -> String {
    // `<` only occurs inside strings, where < means the same. This keeps `</script>` and
    // `<!--` in a function name from ending or confusing the element.
    let text = data.to_compact().replace('<', "\\u003c");
    template.replacen(TOKEN, &text, 1)
}

fn uint(n: u64) -> Json {
    Json::Uint(n)
}

fn column(values: impl Iterator<Item = u64>) -> Json {
    Json::Array(values.map(Json::Uint).collect())
}

fn text_or_null(t: &Option<String>) -> Json {
    t.as_ref().map_or(Json::Null, |t| Json::Str(t.clone()))
}

/// The data the page reads.
pub fn data(a: &Analysis, leaks: &LeakReport) -> Json {
    let obj = |fields: Vec<(&str, Json)>| {
        Json::Object(
            fields
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
        )
    };
    obj(vec![
        ("report_version", uint(REPORT_VERSION)),
        ("summary", json::summary(a, leaks)),
        ("tree", tree(a)),
        ("timeline", timeline(a)),
    ])
}

/// The call tree as parallel arrays indexed by node id. Node 0 is the root; a node's parent
/// always has a lower id, so a consumer can total subtrees with one backwards sweep. Figures are
/// the node's own (not inclusive) and are scaled up for a sampled capture.
fn tree(a: &Analysis) -> Json {
    let n = a.tree.len();
    let scale = a.scale();
    let mut frames: Vec<&FrameInfo> = Vec::new();
    let mut seen: HashMap<&FrameInfo, u64> = HashMap::new();
    let mut frame_of = Vec::with_capacity(n);
    for id in 0..n {
        let f = &a.tree.node(id as u32).frame;
        let idx = *seen.entry(f).or_insert_with(|| {
            frames.push(f);
            frames.len() as u64 - 1
        });
        frame_of.push(idx);
    }
    let frames = frames
        .iter()
        .map(|f| {
            Json::Array(vec![
                Json::Str(f.name.clone()),
                text_or_null(&f.module),
                text_or_null(&f.file),
                uint(u64::from(f.line)),
            ])
        })
        .collect();
    let stat = |pick: fn(&heapscope_analysis::PathStats) -> u64| {
        column(a.stats.iter().map(move |s| pick(s) * scale))
    };
    Json::Object(
        vec![
            // [function, module, file, line]
            ("frames", Json::Array(frames)),
            ("frame", column(frame_of.into_iter())),
            (
                "parent",
                column((0..n).map(|id| u64::from(a.tree.node(id as u32).parent))),
            ),
            ("allocs", stat(|s| s.allocs)),
            ("alloc_bytes", stat(|s| s.alloc_bytes)),
            ("live_allocs", stat(|s| s.live_allocs)),
            ("live_bytes", stat(|s| s.live_bytes)),
            ("peak_bytes", stat(|s| s.peak_live_bytes)),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect(),
    )
}

/// `[ts_ns, live_bytes, live_allocs]` points, oldest first.
fn timeline(a: &Analysis) -> Json {
    let scale = a.scale();
    Json::Array(
        a.timeline
            .iter()
            .map(|p| {
                Json::Array(vec![
                    uint(p.ts_ns),
                    uint(p.live_bytes * scale),
                    uint(p.live_allocs * scale),
                ])
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_template_has_one_data_slot() {
        assert_eq!(TEMPLATE.matches(TOKEN).count(), 1);
        assert!(TEMPLATE
            .contains(r#"<script id="hs-data" type="application/json">{{HS_DATA}}</script>"#));
    }

    #[test]
    fn injected_data_cannot_close_the_script_element() {
        let nasty = Json::Str("</script><!-- <script>".into());
        let page = inject("<script id=\"hs-data\">{{HS_DATA}}</script>", &nasty);
        assert_eq!(page.matches("</script>").count(), 1);
        assert!(!page.contains("<!--"));
        let inner = page
            .strip_prefix("<script id=\"hs-data\">")
            .and_then(|p| p.strip_suffix("</script>"))
            .unwrap();
        assert_eq!(
            Json::parse(inner).unwrap(),
            Json::Str("</script><!-- <script>".into())
        );
    }
}
