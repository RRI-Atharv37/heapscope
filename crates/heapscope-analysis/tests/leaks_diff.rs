//! Leak detection, suppressions and snapshot diff over hand-built traces.

use std::collections::HashMap;

use heapscope_analysis::diff::{self, Change, DiffOptions, Point};
use heapscope_analysis::leaks::{self, ReportKind};
use heapscope_analysis::{
    analyze, Analysis, Frame, FrameResolver, ObjectKind, Options, Suppressions, Trace,
};
use heapscope_trace::{
    AllocKind, CaptureMode, Dropped, Event, HandleKind, Header, StackId, TraceWriter,
};

struct Fake(HashMap<u64, (&'static str, &'static str)>);

impl FrameResolver for Fake {
    fn resolve(&mut self, address: u64) -> Frame {
        match self.0.get(&address) {
            Some((module, function)) => Frame {
                address,
                module: Some((*module).into()),
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

fn resolver() -> Fake {
    Fake(HashMap::from([
        (0x10, ("app.exe", "malloc_base")),
        (0x20, ("app.exe", "operator new")),
        (0x30, ("app.exe", "site_a")),
        (0x31, ("app.exe", "site_b")),
        (0x32, ("app.exe", "build_cache")),
        (0x40, ("app.exe", "main")),
        (0x41, ("app.exe", "`dynamic initializer for 'g_cache''")),
        (0x42, ("app.exe", "open_files")),
    ]))
}

fn header(mode: CaptureMode, attached: bool) -> Header {
    Header {
        tool_version: "test".into(),
        pid: 1,
        exe_path: "app.exe".into(),
        command_line: "app.exe".into(),
        pointer_size: 8,
        start_unix_ns: 0,
        capture_mode: mode,
        min_lifetime_ms: 0,
        attached,
    }
}

fn full() -> Header {
    header(CaptureMode::Full, false)
}

fn alloc(ts_ns: u64, kind: AllocKind, addr: u64, size: u64, stack: StackId) -> Event {
    Event::Alloc {
        ts_ns,
        thread: 1,
        kind,
        addr,
        size,
        requested: size,
        stack,
    }
}

fn heap(ts_ns: u64, addr: u64, size: u64, stack: StackId) -> Event {
    alloc(ts_ns, AllocKind::Heap, addr, size, stack)
}

fn free(ts_ns: u64, addr: u64) -> Event {
    Event::Free {
        ts_ns,
        thread: 1,
        addr,
        stack: 0,
    }
}

fn marker(ts_ns: u64, label: &str) -> Event {
    Event::Marker {
        ts_ns,
        thread: 1,
        label: label.into(),
    }
}

/// Stacks 1..=4: `site_a`, `site_b`, a static initializer, and the handle-opening site.
fn build(header: &Header, events: &[Event]) -> Trace {
    let mut w = TraceWriter::new(Vec::new(), header).unwrap();
    for s in [
        vec![0x10, 0x20, 0x30, 0x40],
        vec![0x10, 0x20, 0x31, 0x40],
        vec![0x10, 0x20, 0x32, 0x41],
        vec![0x42, 0x40],
    ] {
        w.intern_stack(&s);
    }
    for e in events {
        w.write_event(e).unwrap();
    }
    let bytes = w.finish(Dropped::default()).unwrap();
    Trace::read(&bytes[..]).unwrap()
}

fn run(trace: &Trace) -> Analysis {
    analyze(trace, &mut resolver(), &Options::default())
}

fn name_of(a: &Analysis, node: u32) -> &str {
    &a.tree.node(node).frame.name
}

#[test]
fn live_at_exit_is_grouped_by_call_path_and_ordered_by_size() {
    let a = run(&build(
        &full(),
        &[
            heap(100, 0x1000, 64, 1),
            heap(110, 0x2000, 64, 1),
            heap(120, 0x3000, 4096, 2),
            heap(130, 0x4000, 128, 1),
            free(140, 0x4000),
            free(150, 0x3000),
        ],
    ));
    let r = leaks::detect(&a, &Suppressions::none());
    assert_eq!(r.kind, ReportKind::Leaks);
    assert!(!r.estimated);
    assert_eq!(r.entries.len(), 1, "site_b was freed, site_a leaks twice");
    let e = &r.entries[0];
    assert_eq!(name_of(&a, e.node), "site_a");
    assert_eq!((e.allocs, e.bytes, e.kind), (2, 128, ObjectKind::Heap));
    assert_eq!((e.oldest_ts_ns, e.newest_ts_ns), (100, 110));
    assert_eq!((r.total_allocs, r.total_bytes), (2, 128));
    assert_eq!(r.title(), "Leak candidates (live at exit)");
    assert!(r.notes.is_empty(), "{:?}", r.notes);
}

#[test]
fn nothing_live_is_an_empty_report() {
    let a = run(&build(
        &full(),
        &[heap(100, 0x1000, 64, 1), free(110, 0x1000)],
    ));
    let r = leaks::detect(&a, &Suppressions::none());
    assert!(r.entries.is_empty() && r.suppressed.is_empty());
    assert_eq!(r.total_bytes, 0);
}

#[test]
fn objects_are_grouped_by_kind_and_a_realloc_keeps_its_kind() {
    let a = run(&build(
        &full(),
        &[
            heap(100, 0x1000, 64, 1),
            alloc(110, AllocKind::Pool, 0x2000, 1000, 1),
            alloc(120, AllocKind::Virtual, 0x3000, 65536, 2),
            alloc(130, AllocKind::Pool, 0x5000, 100, 2),
            Event::Realloc {
                ts_ns: 140,
                thread: 1,
                old_addr: 0x5000,
                new_addr: 0x6000,
                old_size: 100,
                new_size: 300,
                requested: 300,
                stack: 2,
            },
            Event::HandleOpen {
                ts_ns: 150,
                thread: 1,
                handle: 0x44,
                kind: HandleKind::File,
                stack: 4,
            },
            Event::HandleOpen {
                ts_ns: 151,
                thread: 1,
                handle: 0x48,
                kind: HandleKind::Event,
                stack: 4,
            },
            Event::HandleClose {
                ts_ns: 160,
                thread: 1,
                handle: 0x48,
                stack: 0,
            },
        ],
    ));
    let r = leaks::detect(&a, &Suppressions::none());
    let find = |kind, site: &str| {
        r.entries
            .iter()
            .find(|e| e.kind == kind && name_of(&a, e.node) == site)
    };
    assert_eq!(find(ObjectKind::Heap, "site_a").unwrap().bytes, 64);
    assert_eq!(find(ObjectKind::Pool, "site_a").unwrap().bytes, 1000);
    assert_eq!(find(ObjectKind::Virtual, "site_b").unwrap().bytes, 65536);
    let pool_b = find(ObjectKind::Pool, "site_b").unwrap();
    assert_eq!(
        (pool_b.allocs, pool_b.bytes),
        (1, 300),
        "the resized block is still a pool block"
    );
    let h = find(ObjectKind::Handle, "open_files").unwrap();
    assert_eq!((h.allocs, h.bytes), (1, 0), "one of two handles was closed");

    let kinds: Vec<_> = r.by_kind.iter().map(|k| (k.kind, k.allocs)).collect();
    assert_eq!(
        kinds,
        [
            (ObjectKind::Heap, 1),
            (ObjectKind::Pool, 2),
            (ObjectKind::Virtual, 1),
            (ObjectKind::Handle, 1)
        ]
    );
}

#[test]
fn builtin_suppressions_separate_startup_statics_from_real_candidates() {
    let a = run(&build(
        &full(),
        &[heap(100, 0x1000, 256, 3), heap(110, 0x2000, 64, 1)],
    ));
    let none = leaks::detect(&a, &Suppressions::none());
    assert_eq!(none.entries.len(), 2);

    let r = leaks::detect(&a, &Suppressions::builtin());
    assert_eq!(r.entries.len(), 1);
    assert_eq!(name_of(&a, r.entries[0].node), "site_a");
    assert_eq!(r.suppressed.len(), 1);
    assert_eq!((r.suppressed_allocs, r.suppressed_bytes), (1, 256));
    assert_eq!((r.total_allocs, r.total_bytes), (1, 64));
    let by = r.suppressed[0].suppressed_by.as_ref().unwrap();
    assert!(by.builtin);
    assert_eq!(by.rule, "fn:*dynamic initializer for*");
}

#[test]
fn a_user_suppression_file_is_applied_after_the_builtin_rules() {
    let a = run(&build(
        &full(),
        &[heap(100, 0x1000, 256, 3), heap(110, 0x2000, 64, 1)],
    ));
    let mut s = Suppressions::builtin();
    s.extend(Suppressions::parse("fn:site_a  # kept for the life of the process\n").unwrap());
    let r = leaks::detect(&a, &s);
    assert!(r.entries.is_empty());
    assert_eq!(r.suppressed.len(), 2);
    let user = r
        .suppressed
        .iter()
        .find(|e| name_of(&a, e.node) == "site_a")
        .unwrap();
    let by = user.suppressed_by.as_ref().unwrap();
    assert!(!by.builtin);
    assert_eq!(by.reason, "kept for the life of the process");
}

#[test]
fn a_sampled_capture_is_scaled_and_labelled_estimated() {
    let h = header(CaptureMode::Sampled { one_in: 10 }, false);
    let a = run(&build(
        &h,
        &[heap(100, 0x1000, 64, 1), heap(110, 0x2000, 64, 1)],
    ));
    let r = leaks::detect(&a, &Suppressions::none());
    assert!(r.estimated);
    assert_eq!(r.scale, 10);
    assert_eq!((r.entries[0].allocs, r.entries[0].bytes), (20, 1280));
    assert_eq!(r.title(), "Leak candidates (live at exit, estimated)");
    assert!(
        r.notes.iter().any(|n| n.contains("1 in 10")),
        "{:?}",
        r.notes
    );
}

#[test]
fn handles_are_not_scaled_in_a_sampled_capture() {
    let h = header(CaptureMode::Sampled { one_in: 10 }, false);
    let a = run(&build(
        &h,
        &[Event::HandleOpen {
            ts_ns: 100,
            thread: 1,
            handle: 4,
            kind: HandleKind::File,
            stack: 4,
        }],
    ));
    let r = leaks::detect(&a, &Suppressions::none());
    assert_eq!(r.entries[0].allocs, 1);
}

#[test]
fn an_attached_capture_reports_net_growth_not_leaks() {
    let a = run(&build(
        &header(CaptureMode::Full, true),
        &[heap(100, 0x1000, 64, 1)],
    ));
    let r = leaks::detect(&a, &Suppressions::none());
    assert_eq!(r.kind, ReportKind::NetGrowth);
    assert_eq!(r.title(), "Net growth since attach");
    assert_eq!(r.entries.len(), 1);
    assert!(
        r.notes.iter().any(|n| n.contains("not confirmed leaks")),
        "{:?}",
        r.notes
    );
}

#[test]
fn the_report_follows_the_filter() {
    let t = build(
        &full(),
        &[heap(100, 0x1000, 64, 1), heap(110, 0x2000, 4096, 2)],
    );
    let opts = Options {
        filter: heapscope_analysis::Filter {
            text: Some("site_b".into()),
            ..Default::default()
        },
        ..Options::default()
    };
    let a = analyze(&t, &mut resolver(), &opts);
    let r = leaks::detect(&a, &Suppressions::none());
    assert_eq!(r.entries.len(), 1);
    assert_eq!(name_of(&a, r.entries[0].node), "site_b");
}

fn opts() -> DiffOptions {
    DiffOptions::default()
}

#[test]
fn diff_classifies_new_gone_grew_and_shrank_across_two_runs() {
    // Different addresses and timestamps in the two runs; the call paths are the same.
    let before = run(&build(
        &full(),
        &[
            heap(100, 0x1000, 100, 1), // site_a: 100 -> 300 (grew)
            heap(110, 0x2000, 500, 2), // site_b: 500 -> 200 (shrank)
            heap(120, 0x3000, 77, 3),  // static init: gone
        ],
    ));
    let after = run(&build(
        &full(),
        &[
            heap(5_000, 0x7000_1000, 100, 1),
            heap(5_100, 0x7000_2000, 200, 1),
            heap(5_200, 0x7000_5000, 200, 2),
            alloc(5_300, AllocKind::Heap, 0x7000_9000, 33, 4), // open_files site: new
        ],
    ));
    let d = diff::diff(&before, &after, &opts());
    let get = |name: &str| {
        d.entries
            .iter()
            .find(|e| e.path.starts_with(name))
            .unwrap_or_else(|| panic!("no entry for {name}"))
    };
    let a = get("site_a");
    assert_eq!(a.change, Change::Grew);
    assert_eq!(
        (a.before.bytes, a.after.bytes, a.delta_bytes()),
        (100, 300, 200)
    );
    assert_eq!(a.delta_allocs(), 1);
    assert_eq!(a.location.as_deref(), Some("C:\\src\\site_a.cpp:10"));
    let b = get("site_b");
    assert_eq!(b.change, Change::Shrank);
    assert_eq!(b.delta_bytes(), -300);
    assert_eq!(get("build_cache").change, Change::Gone);
    assert_eq!(get("open_files").change, Change::New);

    assert_eq!(d.entries.len(), 4);
    let sizes: Vec<u64> = d
        .entries
        .iter()
        .map(|e| e.delta_bytes().unsigned_abs())
        .collect();
    assert!(
        sizes.windows(2).all(|w| w[0] >= w[1]),
        "largest change first: {sizes:?}"
    );
    assert_eq!((d.before.bytes, d.after.bytes), (677, 533));
    assert_eq!(d.delta_bytes(), -144);
    assert_eq!(d.count(Change::Grew), 1);
    assert!(d.notes.is_empty(), "{:?}", d.notes);
}

#[test]
fn diff_hides_unchanged_sites_and_small_changes_on_request() {
    let t1 = build(
        &full(),
        &[heap(100, 0x1000, 100, 1), heap(110, 0x2000, 50, 2)],
    );
    let t2 = build(
        &full(),
        &[heap(100, 0x1000, 100, 1), heap(110, 0x2000, 58, 2)],
    );
    let (a, b) = (run(&t1), run(&t2));

    let d = diff::diff(&a, &b, &opts());
    assert_eq!(d.entries.len(), 1, "site_a did not change");
    assert_eq!(d.entries[0].change, Change::Grew);

    let all = diff::diff(
        &a,
        &b,
        &DiffOptions {
            include_unchanged: true,
            ..opts()
        },
    );
    assert_eq!(all.entries.len(), 2);
    assert!(all.entries.iter().any(|e| e.change == Change::Unchanged));

    let quiet = diff::diff(
        &a,
        &b,
        &DiffOptions {
            min_change_bytes: 16,
            ..opts()
        },
    );
    assert!(quiet.entries.is_empty());
}

#[test]
fn diff_of_a_trace_with_itself_is_empty() {
    let a = run(&build(&full(), &[heap(100, 0x1000, 100, 1)]));
    let d = diff::diff(&a, &a, &opts());
    assert!(d.entries.is_empty());
    assert_eq!(d.before, d.after);
}

#[test]
fn diff_scales_sampled_sides_and_says_so() {
    let sampled = header(CaptureMode::Sampled { one_in: 4 }, false);
    let a = run(&build(&sampled, &[heap(100, 0x1000, 100, 1)]));
    let b = run(&build(&full(), &[heap(100, 0x1000, 400, 1)]));
    let d = diff::diff(&a, &b, &opts());
    assert!(d.estimated);
    assert_eq!(d.before.bytes, 400);
    // 100 bytes x 4 equals 400 bytes; only the allocation count (4 estimated vs 1) differs.
    assert!(d.entries.iter().all(|e| e.delta_bytes() == 0));
    assert!(
        d.notes.iter().any(|n| n.contains("different rates")),
        "{:?}",
        d.notes
    );
}

#[test]
fn diff_between_two_markers_of_one_capture() {
    let a = run(&build(
        &full(),
        &[
            heap(100, 0x1000, 10, 1),
            marker(150, "before"),
            heap(200, 0x2000, 200, 1),
            heap(210, 0x3000, 300, 2),
            free(220, 0x1000),
            marker(250, "after"),
            heap(300, 0x4000, 9999, 2),
        ],
    ));
    let from = diff::find_marker(&a, "before").unwrap();
    let to = diff::find_marker(&a, "after").unwrap();
    let d = diff::diff_points(&a, Point::Marker(from), Point::Marker(to), &opts()).unwrap();
    assert_eq!((d.before.bytes, d.after.bytes), (10, 500));
    let site = |n: &str| d.entries.iter().find(|e| e.path.starts_with(n)).unwrap();
    assert_eq!(site("site_a").after.bytes, 200);
    assert_eq!(site("site_a").change, Change::Grew);
    assert_eq!(site("site_b").change, Change::New);

    // Against the end of the trace the late allocation shows up as well.
    let d = diff::diff_points(&a, Point::Marker(to), Point::End, &opts()).unwrap();
    assert_eq!(d.entries.len(), 1);
    assert_eq!(d.entries[0].delta_bytes(), 9999);

    assert!(diff::find_marker(&a, "nope").is_none());
    assert!(diff::diff_points(&a, Point::Marker(7), Point::End, &opts()).is_err());
}

#[test]
fn paths_that_differ_only_below_the_identity_depth_merge() {
    // Both stacks reach site_a, one through main and one through a thread entry.
    let mut w = TraceWriter::new(Vec::new(), &full()).unwrap();
    w.intern_stack(&[0x10, 0x20, 0x30, 0x40]);
    w.intern_stack(&[0x10, 0x20, 0x30, 0x42]);
    w.write_event(&heap(100, 0x1000, 10, 1)).unwrap();
    w.write_event(&heap(110, 0x2000, 20, 2)).unwrap();
    let bytes = w.finish(Dropped::default()).unwrap();
    let a = run(&Trace::read(&bytes[..]).unwrap());
    let empty = run(&build(&full(), &[]));

    let deep = diff::diff(&empty, &a, &opts());
    assert_eq!(deep.entries.len(), 2);
    let shallow = diff::diff(&empty, &a, &DiffOptions { depth: 1, ..opts() });
    assert_eq!(shallow.entries.len(), 1);
    assert_eq!(shallow.entries[0].after.bytes, 30);
}
