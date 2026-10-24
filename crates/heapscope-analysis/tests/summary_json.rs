//! The JSON summary: schema v1 key set (frozen), exactness flags and sampled scaling.

use std::collections::HashMap;

use heapscope_analysis::json::{self, Json};
use heapscope_analysis::leaks;
use heapscope_analysis::{analyze, Frame, FrameResolver, Options, Suppressions, Trace};
use heapscope_trace::{AllocKind, CaptureMode, Dropped, Event, Header, TraceWriter};

struct Fake(HashMap<u64, &'static str>);

impl FrameResolver for Fake {
    fn resolve(&mut self, address: u64) -> Frame {
        match self.0.get(&address) {
            Some(function) => Frame {
                address,
                module: Some("app.exe".into()),
                offset: address & 0xfff,
                function: Some((*function).into()),
                file: Some(format!("C:\\src\\{function}.cpp")),
                line: 10,
            },
            None => Frame {
                address,
                ..Frame::default()
            },
        }
    }
}

fn header(mode: CaptureMode, attached: bool) -> Header {
    Header {
        tool_version: "test".into(),
        pid: 7,
        exe_path: "C:\\app\\app.exe".into(),
        command_line: "app.exe \"quoted\"".into(),
        pointer_size: 8,
        start_unix_ns: 0,
        capture_mode: mode,
        min_lifetime_ms: 0,
        attached,
    }
}

/// Two sites: `site_a` leaks 3 x 100 bytes, `site_b` allocates and frees 2 x 50 bytes.
fn summary_of(mode: CaptureMode, attached: bool) -> Json {
    let mut w = TraceWriter::new(Vec::new(), &header(mode, attached)).unwrap();
    let a = w.intern_stack(&[0x10, 0x20, 0x30, 0x40]);
    let b = w.intern_stack(&[0x10, 0x20, 0x31, 0x40]);
    let mut ts = 0;
    let mut ev = |e: Event| {
        ts += 1;
        e.with_ts(ts)
    };
    let mut events = Vec::new();
    for i in 0..3 {
        events.push(ev(Event::Alloc {
            ts_ns: 0,
            thread: 1,
            kind: AllocKind::Heap,
            addr: 0x1000 + i * 0x100,
            size: 100,
            requested: 100,
            stack: a,
        }));
    }
    for i in 0..2 {
        let addr = 0x9000 + i * 0x100;
        events.push(ev(Event::Alloc {
            ts_ns: 0,
            thread: 1,
            kind: AllocKind::Heap,
            addr,
            size: 50,
            requested: 50,
            stack: b,
        }));
        events.push(ev(Event::Free {
            ts_ns: 0,
            thread: 1,
            addr,
            stack: 0,
        }));
    }
    events.push(ev(Event::Marker {
        ts_ns: 0,
        thread: 1,
        label: "mid".into(),
    }));
    for e in &events {
        w.write_event(e).unwrap();
    }
    let trace = Trace::read(&w.finish(Dropped::default()).unwrap()[..]).unwrap();
    let mut resolver = Fake(HashMap::from([
        (0x10, "malloc_base"),
        (0x20, "operator new"),
        (0x30, "site_a"),
        (0x31, "site_b"),
        (0x40, "main"),
    ]));
    let analysis = analyze(&trace, &mut resolver, &Options::default());
    let report = leaks::detect(&analysis, &Suppressions::builtin());
    // Through text, as a consumer sees it.
    Json::parse(&json::summary(&analysis, &report).to_pretty()).unwrap()
}

trait WithTs {
    fn with_ts(self, ts: u64) -> Event;
}

impl WithTs for Event {
    fn with_ts(mut self, ts: u64) -> Event {
        match &mut self {
            Event::Alloc { ts_ns, .. }
            | Event::Free { ts_ns, .. }
            | Event::Marker { ts_ns, .. } => *ts_ns = ts * 1000,
            _ => unreachable!(),
        }
        self
    }
}

fn u(v: &Json, path: &[&str]) -> u64 {
    let mut cur = v;
    for k in path {
        cur = cur.get(k).unwrap_or_else(|| panic!("missing {k}"));
    }
    cur.as_u64()
        .unwrap_or_else(|| panic!("{path:?} is not a number"))
}

/// Changing this list is a schema change: bump `schema_version` and document it.
#[test]
fn schema_v1_key_set_is_frozen() {
    let j = summary_of(CaptureMode::Full, false);
    assert_eq!(
        j.keys(),
        [
            "schema_version",
            "tool",
            "capture",
            "totals",
            "peak",
            "sites",
            "leaks",
            "churn",
            "markers",
            "warnings"
        ]
    );
    assert_eq!(
        j.get("capture").unwrap().keys(),
        [
            "mode",
            "sample_rate",
            "min_lifetime_ms",
            "attached",
            "estimated",
            "complete",
            "exe",
            "command_line",
            "pid",
            "pointer_size",
            "dropped"
        ]
    );
    assert_eq!(
        j.get("capture").unwrap().get("dropped").unwrap().keys(),
        ["overflow", "min_lifetime", "sampling"]
    );
    assert_eq!(
        j.get("totals").unwrap().keys(),
        [
            "allocs",
            "alloc_bytes",
            "frees",
            "freed_bytes",
            "live_allocs",
            "live_bytes",
            "unmatched_frees",
            "missed_frees"
        ]
    );
    assert_eq!(
        j.get("peak").unwrap().keys(),
        ["ts_ns", "live_bytes", "live_allocs"]
    );
    let site = &j.get("sites").unwrap().as_array().unwrap()[0];
    assert_eq!(
        site.keys(),
        [
            "id",
            "function",
            "module",
            "file",
            "line",
            "path",
            "allocs",
            "alloc_bytes",
            "frees",
            "freed_bytes",
            "live_allocs",
            "live_bytes",
            "peak_live_bytes",
            "short_lived_allocs"
        ]
    );
    let leaks = j.get("leaks").unwrap();
    assert_eq!(
        leaks.keys(),
        [
            "kind",
            "title",
            "estimated",
            "total_allocs",
            "total_bytes",
            "suppressed_allocs",
            "suppressed_bytes",
            "by_kind",
            "entries",
            "suppressed",
            "notes"
        ]
    );
    let entry = &leaks.get("entries").unwrap().as_array().unwrap()[0];
    assert_eq!(
        entry.keys(),
        [
            "id",
            "function",
            "module",
            "file",
            "line",
            "path",
            "kind",
            "allocs",
            "bytes",
            "oldest_ts_ns",
            "newest_ts_ns"
        ]
    );
    let marker = &j.get("markers").unwrap().as_array().unwrap()[0];
    assert_eq!(
        marker.keys(),
        ["label", "ts_ns", "live_bytes", "live_allocs"]
    );
}

#[test]
fn full_capture_numbers_are_exact_and_sites_are_ordered() {
    let j = summary_of(CaptureMode::Full, false);
    assert_eq!(u(&j, &["schema_version"]), 1);
    let cap = j.get("capture").unwrap();
    assert_eq!(cap.get("mode").and_then(Json::as_str), Some("full"));
    assert_eq!(cap.get("estimated"), Some(&Json::Bool(false)));
    assert_eq!(u(&j, &["capture", "sample_rate"]), 1);

    assert_eq!(u(&j, &["totals", "allocs"]), 5);
    assert_eq!(u(&j, &["totals", "alloc_bytes"]), 400);
    assert_eq!(u(&j, &["totals", "frees"]), 2);
    assert_eq!(u(&j, &["totals", "live_bytes"]), 300);
    // The first block of site_b is live next to all three of site_a's.
    assert_eq!(u(&j, &["peak", "live_bytes"]), 350);

    let sites = j.get("sites").unwrap().as_array().unwrap();
    let names: Vec<_> = sites
        .iter()
        .map(|s| s.get("function").and_then(Json::as_str).unwrap())
        .collect();
    assert_eq!(names, ["site_a", "site_b"], "largest first, helpers hidden");
    assert_eq!(u(&sites[0], &["live_bytes"]), 300);
    assert_eq!(u(&sites[1], &["frees"]), 2);
    assert_eq!(
        sites[0].get("id").and_then(Json::as_str),
        Some("app.exe!site_a <- app.exe!main")
    );

    let leaks = j.get("leaks").unwrap();
    assert_eq!(leaks.get("kind").and_then(Json::as_str), Some("leaks"));
    assert_eq!(u(&j, &["leaks", "total_bytes"]), 300);
    assert_eq!(leaks.get("entries").unwrap().as_array().unwrap().len(), 1);
    let m = &j.get("markers").unwrap().as_array().unwrap()[0];
    assert_eq!(m.get("label").and_then(Json::as_str), Some("mid"));
    assert_eq!(u(m, &["live_bytes"]), 300);
}

#[test]
fn sampled_numbers_are_scaled_and_labelled_estimated() {
    let j = summary_of(CaptureMode::Sampled { one_in: 10 }, false);
    let cap = j.get("capture").unwrap();
    assert_eq!(cap.get("mode").and_then(Json::as_str), Some("sampled"));
    assert_eq!(cap.get("estimated"), Some(&Json::Bool(true)));
    assert_eq!(u(&j, &["capture", "sample_rate"]), 10);
    assert_eq!(u(&j, &["totals", "alloc_bytes"]), 4000);
    assert_eq!(u(&j, &["leaks", "total_bytes"]), 3000);
    assert_eq!(u(&j, &["leaks", "total_allocs"]), 30);
    assert_eq!(
        j.get("leaks").unwrap().get("estimated"),
        Some(&Json::Bool(true))
    );
    let sites = j.get("sites").unwrap().as_array().unwrap();
    assert_eq!(u(&sites[0], &["live_bytes"]), 3000);
}

#[test]
fn attached_capture_is_net_growth_and_not_exact() {
    let j = summary_of(CaptureMode::Full, true);
    let cap = j.get("capture").unwrap();
    assert_eq!(cap.get("mode").and_then(Json::as_str), Some("attached"));
    assert_eq!(cap.get("attached"), Some(&Json::Bool(true)));
    assert_eq!(cap.get("estimated"), Some(&Json::Bool(true)));
    assert_eq!(
        j.get("leaks").unwrap().get("kind").and_then(Json::as_str),
        Some("net_growth")
    );
    assert!(!j.get("warnings").unwrap().as_array().unwrap().is_empty());
}
