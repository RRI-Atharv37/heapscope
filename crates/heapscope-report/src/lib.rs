//! The HTML report: one self-contained page (no network, no external files) holding the summary,
//! call tree and flame graph of a trace.
//!
//! The page itself is built from `report/` (TypeScript) and committed as
//! `assets/report-template.html`, so a Rust build needs no Node. `render` injects the data of an
//! [`Analysis`] into that template's `hs-data` script element.
//!
//! The embedded data is `report_version` 1: the frozen summary (`docs/json-schema.md`) under
//! `summary`, plus the whole call tree in columns under `tree`, the timeline and the live set at
//! each marker. A diff report (`render_diff`) carries a `diff` object instead: the merged
//! call tree of both sides with live figures before and after. The data is private to the page
//! and is allowed to change with the template; scripts should read `heapscope analyze -o`
//! instead.

use std::collections::HashMap;

use heapscope_analysis::diff::{self as diffing, Change, Diff, Point};
use heapscope_analysis::identity;
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

fn obj(fields: Vec<(&str, Json)>) -> Json {
    Json::Object(
        fields
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
    )
}

/// The data the page reads for one capture.
pub fn data(a: &Analysis, leaks: &LeakReport) -> Json {
    obj(vec![
        ("report_version", uint(REPORT_VERSION)),
        ("summary", json::summary(a, leaks)),
        ("tree", tree(a)),
        ("timeline", timeline(a)),
        ("snapshots", snapshots(a)),
    ])
}

/// Distinct frames, in first-use order, as `[function, module, file, line]` rows.
#[derive(Default)]
struct FrameTable<'a> {
    frames: Vec<&'a FrameInfo>,
    seen: HashMap<&'a FrameInfo, u64>,
}

impl<'a> FrameTable<'a> {
    fn index(&mut self, f: &'a FrameInfo) -> u64 {
        let frames = &mut self.frames;
        *self.seen.entry(f).or_insert_with(|| {
            frames.push(f);
            frames.len() as u64 - 1
        })
    }

    fn json(&self) -> Json {
        Json::Array(
            self.frames
                .iter()
                .map(|f| {
                    Json::Array(vec![
                        Json::Str(f.name.clone()),
                        text_or_null(&f.module),
                        text_or_null(&f.file),
                        uint(u64::from(f.line)),
                    ])
                })
                .collect(),
        )
    }
}

/// The call tree as parallel arrays indexed by node id. Node 0 is the root; a node's parent
/// always has a lower id, so a consumer can total subtrees with one backwards sweep. Figures are
/// the node's own (not inclusive) and are scaled up for a sampled capture.
fn tree(a: &Analysis) -> Json {
    let n = a.tree.len();
    let scale = a.scale();
    let mut table = FrameTable::default();
    let frame_of: Vec<u64> = (0..n)
        .map(|id| table.index(&a.tree.node(id as u32).frame))
        .collect();
    let stat = |pick: fn(&heapscope_analysis::PathStats) -> u64| {
        column(a.stats.iter().map(move |s| pick(s) * scale))
    };
    obj(vec![
        // [function, module, file, line]
        ("frames", table.json()),
        ("frame", column(frame_of.into_iter())),
        (
            "parent",
            column((0..n).map(|id| u64::from(a.tree.node(id as u32).parent))),
        ),
        ("allocs", stat(|s| s.allocs)),
        ("alloc_bytes", stat(|s| s.alloc_bytes)),
        ("live_allocs", stat(|s| s.live_allocs)),
        ("live_bytes", stat(|s| s.live_bytes)),
        ("peak_allocs", stat(|s| s.peak_live_allocs)),
        ("peak_bytes", stat(|s| s.peak_live_bytes)),
    ])
}

/// The live set at each marker, parallel to `summary.markers`, as columns over the tree's node
/// ids: `{nodes, allocs, bytes}` listing only nodes with live memory.
fn snapshots(a: &Analysis) -> Json {
    let scale = a.scale();
    Json::Array(
        a.snapshots
            .iter()
            .map(|s| {
                obj(vec![
                    ("nodes", column(s.live.iter().map(|l| u64::from(l.0)))),
                    ("allocs", column(s.live.iter().map(|l| l.1 * scale))),
                    ("bytes", column(s.live.iter().map(|l| l.2 * scale))),
                ])
            })
            .collect(),
    )
}

/// One side of a diff report: an analysis and the moment of it that is compared.
pub struct Side<'a> {
    pub analysis: &'a Analysis,
    pub point: Point,
    /// Shown in the page, for example the trace file name.
    pub label: String,
}

/// A complete HTML page for a diff of two sides.
pub fn render_diff(before: &Side<'_>, after: &Side<'_>, diff: &Diff) -> Result<String, String> {
    Ok(inject(TEMPLATE, &diff_data(before, after, diff)?))
}

/// Most changed sites listed flat on the diff summary; the delta tree has them all.
const DIFF_ENTRIES: usize = 200;

/// The data the page reads for a diff: the delta tree, and the flat list `heapscope diff` prints.
pub fn diff_data<'a>(before: &Side<'a>, after: &Side<'a>, diff: &Diff) -> Result<Json, String> {
    let live = |side: &Side<'_>| -> Result<Vec<(u32, u64, u64)>, String> {
        let scale = side.analysis.scale();
        Ok(diffing::live_at(side.analysis, side.point)?
            .into_iter()
            .map(|(n, allocs, bytes)| (n, allocs * scale, bytes * scale))
            .collect())
    };
    let (live_before, live_after) = (live(before)?, live(after)?);

    // Merge the two call trees on the frames' module and function names, so a site matches
    // across runs and builds (the same rule as `heapscope diff`). Nodes are added parent first,
    // which keeps every parent's id below its children's.
    let root = FrameInfo {
        name: String::new(),
        module: None,
        file: None,
        line: 0,
    };
    let mut table = FrameTable::default();
    let mut frame_of: Vec<u64> = vec![table.index(&root)];
    let mut parent: Vec<u64> = vec![0];
    let mut index: HashMap<(u64, String), u64> = HashMap::new();
    let mut map_side = |side: &Side<'a>| -> Vec<u64> {
        let tree = &side.analysis.tree;
        let mut to = vec![0u64; tree.len()];
        for id in 1..tree.len() {
            let node = tree.node(id as u32);
            let p = to[node.parent as usize];
            let key = (p, identity::frame_key(&node.frame));
            to[id] = *index.entry(key).or_insert_with(|| {
                frame_of.push(table.index(&node.frame));
                parent.push(p);
                frame_of.len() as u64 - 1
            });
        }
        to
    };
    // The second side first, so a site's file and line come from the newer build when both have it.
    let to_after = map_side(after);
    let to_before = map_side(before);

    let n = frame_of.len();
    let mut columns = [vec![0u64; n], vec![0u64; n], vec![0u64; n], vec![0u64; n]];
    for &(node, allocs, bytes) in &live_before {
        let m = to_before[node as usize] as usize;
        columns[0][m] += allocs;
        columns[1][m] += bytes;
    }
    for &(node, allocs, bytes) in &live_after {
        let m = to_after[node as usize] as usize;
        columns[2][m] += allocs;
        columns[3][m] += bytes;
    }
    let [before_allocs, before_bytes, after_allocs, after_bytes] = columns;

    let live_json = |l: &diffing::Live| Json::Array(vec![uint(l.allocs), uint(l.bytes)]);
    let entries = diff
        .entries
        .iter()
        .take(DIFF_ENTRIES)
        .map(|e| {
            obj(vec![
                ("change", Json::Str(e.change.label().to_string())),
                ("path", Json::Str(e.path.clone())),
                ("location", text_or_null(&e.location)),
                ("before", live_json(&e.before)),
                ("after", live_json(&e.after)),
            ])
        })
        .collect();
    let side_json = |side: &Side<'_>, total: &diffing::Live| {
        obj(vec![
            ("label", Json::Str(side.label.clone())),
            ("at", Json::Str(point_label(side))),
            ("live_allocs", uint(total.allocs)),
            ("live_bytes", uint(total.bytes)),
            ("capture", json::capture(&side.analysis.capture)),
        ])
    };
    let count = |c: Change| uint(diff.count(c) as u64);

    Ok(obj(vec![
        ("report_version", uint(REPORT_VERSION)),
        (
            "diff",
            obj(vec![
                ("before", side_json(before, &diff.before)),
                ("after", side_json(after, &diff.after)),
                ("estimated", Json::Bool(diff.estimated)),
                (
                    "notes",
                    Json::Array(diff.notes.iter().map(|n| Json::Str(n.clone())).collect()),
                ),
                (
                    "counts",
                    obj(vec![
                        ("grew", count(Change::Grew)),
                        ("new", count(Change::New)),
                        ("shrank", count(Change::Shrank)),
                        ("gone", count(Change::Gone)),
                    ]),
                ),
                ("entries", Json::Array(entries)),
                ("entries_total", uint(diff.entries.len() as u64)),
                (
                    "tree",
                    obj(vec![
                        ("frames", table.json()),
                        ("frame", column(frame_of.into_iter())),
                        ("parent", column(parent.into_iter())),
                        ("before_allocs", column(before_allocs.into_iter())),
                        ("before_bytes", column(before_bytes.into_iter())),
                        ("after_allocs", column(after_allocs.into_iter())),
                        ("after_bytes", column(after_bytes.into_iter())),
                    ]),
                ),
            ]),
        ),
    ]))
}

fn point_label(side: &Side<'_>) -> String {
    match side.point {
        Point::End => "end of trace".to_string(),
        Point::Marker(i) => side.analysis.markers.get(i).map_or_else(
            || format!("marker {i}"),
            |m| format!("marker '{}'", m.label),
        ),
    }
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
