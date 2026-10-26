//! Analysis over hand-built traces, with a fake symbol resolver so no PDBs are involved.

use std::collections::HashMap;

use heapscope_analysis::{analyze, Analysis, Filter, Frame, FrameResolver, NodeId, Options, Trace};
use heapscope_trace::{
    AllocKind, CaptureMode, ChurnSummary, Dropped, Event, Header, StackId, TraceWriter,
};

const MS: u64 = 1_000_000;

struct Fake(HashMap<u64, (&'static str, &'static str)>);

impl FrameResolver for Fake {
    fn resolve(&mut self, address: u64) -> Frame {
        match self.0.get(&address) {
            Some((module, function)) => Frame {
                address,
                module: Some((*module).into()),
                offset: address & 0xfff,
                function: (!function.is_empty()).then(|| (*function).into()),
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
        (0x32, ("app.exe", "site_c")),
        (0x40, ("app.exe", "main")),
        (0x50, ("ntdll.dll", "")),
    ]))
}

fn header() -> Header {
    Header {
        tool_version: "test".into(),
        pid: 1,
        exe_path: "app.exe".into(),
        command_line: "app.exe".into(),
        pointer_size: 8,
        start_unix_ns: 0,
        capture_mode: CaptureMode::Full,
        min_lifetime_ms: 0,
        attached: false,
    }
}

fn alloc(ts_ns: u64, thread: u32, addr: u64, size: u64, stack: StackId) -> Event {
    Event::Alloc {
        ts_ns,
        thread,
        kind: AllocKind::Heap,
        addr,
        size,
        requested: size,
        stack,
    }
}

fn free(ts_ns: u64, addr: u64) -> Event {
    Event::Free {
        ts_ns,
        thread: 1,
        addr,
        stack: 0,
    }
}

/// Stacks 1..=3 reach `site_a`, `site_b` and `site_c` through the allocator frames.
fn stacks() -> Vec<Vec<u64>> {
    vec![
        vec![0x10, 0x20, 0x30, 0x40],
        vec![0x10, 0x20, 0x31, 0x40],
        vec![0x50, 0x32, 0x40],
    ]
}

fn build(header: &Header, events: &[Event]) -> Trace {
    let mut w = TraceWriter::new(Vec::new(), header).unwrap();
    for s in stacks() {
        w.intern_stack(&s);
    }
    for e in events {
        w.write_event(e).unwrap();
    }
    let bytes = w.finish(Dropped::default()).unwrap();
    Trace::read(&bytes[..]).unwrap()
}

fn run(trace: &Trace, opts: &Options) -> Analysis {
    analyze(trace, &mut resolver(), opts)
}

fn site(a: &Analysis, name: &str) -> NodeId {
    (1..a.tree.len() as NodeId)
        .find(|&i| a.tree.node(i).frame.name == name && a.stats[i as usize].allocs > 0)
        .unwrap_or_else(|| panic!("no allocation site {name}"))
}

#[test]
fn allocator_frames_are_hidden_and_sites_are_keyed_by_the_first_user_frame() {
    let t = build(
        &header(),
        &[
            alloc(100, 1, 0x1000, 64, 1),
            alloc(200, 1, 0x2000, 64, 1),
            alloc(300, 1, 0x3000, 4096, 2),
            free(400, 0x1000),
        ],
    );
    let a = run(&t, &Options::default());
    let sa = site(&a, "site_a");
    let sb = site(&a, "site_b");
    assert_ne!(sa, sb);
    assert_eq!(a.tree.path_string(sa, 8), "site_a <- main");
    let st = a.stats[sa as usize];
    assert_eq!(
        (st.allocs, st.alloc_bytes, st.frees, st.freed_bytes),
        (2, 128, 1, 64)
    );
    assert_eq!((st.live_allocs, st.live_bytes), (1, 64));
    let st = a.stats[sb as usize];
    assert_eq!((st.allocs, st.live_bytes), (1, 4096));
    assert_eq!(a.totals.allocs, 3);
    assert_eq!(a.totals.live_bytes, 64 + 4096);
    assert!(a.totals.live_allocs == 2 && a.totals.unmatched_frees == 0);
    // No allocator frame appears anywhere in the tree.
    for i in 1..a.tree.len() as NodeId {
        let n = &a.tree.node(i).frame.name;
        assert!(n != "malloc_base" && n != "operator new", "{n}");
    }
}

#[test]
fn unresolved_system_frames_at_the_inner_end_are_trimmed_and_the_site_is_the_user_frame() {
    let t = build(&header(), &[alloc(100, 1, 0x1000, 8, 3)]);
    let a = run(&t, &Options::default());
    assert_eq!(a.tree.path_string(site(&a, "site_c"), 8), "site_c <- main");
}

#[test]
fn untrimmed_analysis_keeps_every_frame() {
    let t = build(&header(), &[alloc(100, 1, 0x1000, 8, 1)]);
    let a = run(
        &t,
        &Options {
            trim_frames: false,
            ..Options::default()
        },
    );
    let leaf = (1..a.tree.len() as NodeId)
        .find(|&i| a.stats[i as usize].allocs > 0)
        .unwrap();
    assert_eq!(a.tree.node(leaf).frame.name, "malloc_base");
}

fn pool(ts_ns: u64, addr: u64, size: u64, stack: StackId) -> Event {
    Event::Alloc {
        ts_ns,
        thread: 1,
        kind: AllocKind::Pool,
        addr,
        size,
        requested: size,
        stack,
    }
}

fn pool_free(ts_ns: u64, addr: u64) -> Event {
    Event::PoolFree {
        ts_ns,
        thread: 1,
        addr,
        stack: 0,
    }
}

/// A pool object may sit at the address of the heap chunk that contains it (slot 0 of a fixed
/// pool). The two are different objects: the chunk's free must not remove the leaked slot, and
/// the slot's allocation must not replace the chunk.
#[test]
fn pool_object_at_its_chunk_address_is_a_separate_live_object() {
    let t = build(
        &header(),
        &[
            alloc(10, 1, 0x10000, 65_536, 1),
            pool(20, 0x10000, 64, 2),
            pool(30, 0x10040, 100, 2),
            pool_free(40, 0x10040),
            free(50, 0x10000),
        ],
    );
    let a = run(&t, &Options::default());
    assert_eq!(a.totals.missed_frees, 0);
    assert_eq!(a.totals.unmatched_frees, 0);
    assert_eq!(
        (a.peak.ts_ns, a.peak.live_bytes, a.peak.live_allocs),
        (30, 65_536 + 64 + 100, 3)
    );
    let r = heapscope_analysis::leaks::detect(&a, &heapscope_analysis::Suppressions::none());
    assert_eq!(r.entries.len(), 1, "{:?}", r.entries);
    assert_eq!((r.entries[0].allocs, r.entries[0].bytes), (1, 64));
}

#[test]
fn peak_is_found_with_its_live_set() {
    let t = build(
        &header(),
        &[
            alloc(100, 1, 0x1000, 100, 1),
            alloc(200, 1, 0x2000, 200, 2),
            free(300, 0x1000),
            alloc(400, 1, 0x3000, 50, 1),
            free(500, 0x2000),
        ],
    );
    let a = run(&t, &Options::default());
    assert_eq!(
        (a.peak.ts_ns, a.peak.live_bytes, a.peak.live_allocs),
        (200, 300, 2)
    );
    let (sa, sb) = (site(&a, "site_a"), site(&a, "site_b"));
    assert_eq!(a.stats[sa as usize].peak_live_bytes, 100);
    assert_eq!(a.stats[sb as usize].peak_live_bytes, 200);
    // At the end only the 50-byte block is left.
    assert_eq!(
        (
            a.stats[sa as usize].live_bytes,
            a.stats[sb as usize].live_bytes
        ),
        (50, 0)
    );
    assert!(a.timeline.windows(2).all(|w| w[0].ts_ns <= w[1].ts_ns));
    assert_eq!(a.timeline.iter().map(|p| p.live_bytes).max(), Some(300));
}

#[test]
fn events_out_of_order_across_threads_are_replayed_in_time_order() {
    // The free of 0x1000 is written before its allocation (different threads' buffers).
    let t = build(
        &header(),
        &[
            free(300, 0x1000),
            alloc(100, 1, 0x1000, 64, 1),
            alloc(200, 2, 0x2000, 32, 1),
        ],
    );
    let a = run(&t, &Options::default());
    assert_eq!(a.totals.unmatched_frees, 0);
    assert_eq!(a.totals.live_bytes, 32);
}

#[test]
fn realloc_moves_the_block_to_the_reallocating_site() {
    let t = build(
        &header(),
        &[
            alloc(100, 1, 0x1000, 64, 1),
            Event::Realloc {
                ts_ns: 200,
                thread: 1,
                old_addr: 0x1000,
                new_addr: 0x5000,
                old_size: 64,
                new_size: 256,
                requested: 256,
                stack: 2,
            },
        ],
    );
    let a = run(&t, &Options::default());
    let (sa, sb) = (site(&a, "site_a"), site(&a, "site_b"));
    assert_eq!(a.stats[sa as usize].live_bytes, 0);
    assert_eq!(a.stats[sb as usize].live_bytes, 256);
    assert_eq!(a.totals.live_bytes, 256);
}

#[test]
fn a_free_of_unknown_memory_is_counted_not_fatal() {
    let t = build(
        &header(),
        &[free(100, 0xdead0), alloc(200, 1, 0x1000, 8, 1)],
    );
    let a = run(&t, &Options::default());
    assert_eq!(a.totals.unmatched_frees, 1);
    assert_eq!(a.totals.live_bytes, 8);
}

#[test]
fn markers_record_the_live_size_at_their_moment() {
    let t = build(
        &header(),
        &[
            alloc(100, 1, 0x1000, 100, 1),
            Event::Marker {
                ts_ns: 150,
                thread: 1,
                label: "after-load".into(),
            },
            alloc(200, 1, 0x2000, 400, 1),
            free(300, 0x1000),
            Event::Marker {
                ts_ns: 900,
                thread: 1,
                label: "end".into(),
            },
        ],
    );
    let a = run(&t, &Options::default());
    let got: Vec<_> = a
        .markers
        .iter()
        .map(|m| (m.label.as_str(), m.live_bytes))
        .collect();
    assert_eq!(got, [("after-load", 100), ("end", 400)]);
}

/// A burst of `n` allocations at `stack`, each freed `life` ns after it was made.
fn burst(n: u64, stack: StackId, life: u64, base_addr: u64) -> Vec<Event> {
    let mut v = Vec::new();
    for i in 0..n {
        let ts = 1_000 + i * 50_000;
        v.push(alloc(ts, 1, base_addr + i * 16, 48, stack));
        v.push(free(ts + life, base_addr + i * 16));
    }
    v
}

#[test]
fn min_lifetime_summaries_give_the_same_churn_totals_as_a_full_capture() {
    // Full capture: 1000 short-lived (20 us) allocations at site_a and 10 long-lived ones.
    let mut full = burst(1000, 1, 20_000, 0x10_0000);
    full.extend((0..10).map(|i| alloc(60_000_000 + i, 1, 0x90_0000 + i * 16, 48, 1)));
    full.extend((0..10).map(|i| free(90 * MS + i, 0x90_0000 + i * 16)));
    let t_full = build(&header(), &full);

    // The same run seen through --min-lifetime 1ms: the short ones exist only as a summary,
    // delivered in two pieces (two threads).
    let mut folded: Vec<Event> = (0..10)
        .map(|i| alloc(60_000_000 + i, 1, 0x90_0000 + i * 16, 48, 1))
        .collect();
    folded.extend((0..10).map(|i| free(90 * MS + i, 0x90_0000 + i * 16)));
    for part in 0..2u64 {
        folded.push(Event::ChurnSummary {
            ts_ns: 100 * MS,
            summary: ChurnSummary {
                stack: 1,
                allocs: 500,
                bytes: 500 * 48,
                total_lifetime_ns: 500 * 20_000,
                max_concurrent: 1 + part,
            },
        });
    }
    let mut h = header();
    h.min_lifetime_ms = 1;
    let t_fold = build(&h, &folded);

    let a_full = run(&t_full, &Options::default());
    let a_fold = run(&t_fold, &Options::default());
    let (f, g) = (
        a_full.stats[site(&a_full, "site_a") as usize],
        a_fold.stats[site(&a_fold, "site_a") as usize],
    );
    assert_eq!(f.allocs, 1010);
    assert_eq!(
        (f.allocs, f.alloc_bytes, f.frees),
        (g.allocs, g.alloc_bytes, g.frees)
    );
    assert_eq!(
        (f.short_allocs, f.short_bytes, f.short_lifetime_ns),
        (g.short_allocs, g.short_bytes, g.short_lifetime_ns)
    );
    assert_eq!(f.lifetime_ns, g.lifetime_ns, "total lifetime");
    assert_eq!(
        (a_full.totals.allocs, a_full.totals.alloc_bytes),
        (a_fold.totals.allocs, a_fold.totals.alloc_bytes)
    );
    assert_eq!(
        a_fold.short_lived_ns, MS,
        "threshold follows the capture's --min-lifetime"
    );

    let c = &a_fold.churn[0];
    assert_eq!(
        (c.allocs, c.bytes, c.avg_lifetime_ns),
        (1000, 48_000, 20_000)
    );
    assert!(c.share > 0.98 && c.max_concurrent == 2);
    assert!(a_fold.warnings.iter().any(|w| w.contains("--min-lifetime")));
}

#[test]
fn churn_threshold_can_be_overridden() {
    let t = build(&header(), &burst(10, 1, 3 * MS, 0x1000));
    let default = run(&t, &Options::default());
    assert!(
        default.churn.is_empty(),
        "3 ms is not short at the 1 ms default"
    );
    let a = run(
        &t,
        &Options {
            short_lived_ns: Some(5 * MS),
            ..Options::default()
        },
    );
    assert_eq!(a.churn.len(), 1);
    assert_eq!(a.churn[0].allocs, 10);
}

#[test]
fn filters_restrict_what_is_counted() {
    let t = build(
        &header(),
        &[
            alloc(100, 1, 0x1000, 10, 1),
            alloc(110, 2, 0x2000, 1000, 1),
            alloc(120, 2, 0x3000, 20, 2),
            free(200, 0x1000),
            free(210, 0x2000),
        ],
    );
    let by_thread = run(
        &t,
        &Options {
            filter: Filter {
                threads: vec![2],
                ..Filter::default()
            },
            ..Options::default()
        },
    );
    assert_eq!(
        (by_thread.totals.allocs, by_thread.totals.alloc_bytes),
        (2, 1020)
    );
    assert_eq!(
        by_thread.totals.unmatched_frees, 0,
        "frees of filtered allocations are not noise"
    );
    assert_eq!(by_thread.totals.live_bytes, 20);

    let by_size = run(
        &t,
        &Options {
            filter: Filter {
                min_size: Some(100),
                ..Filter::default()
            },
            ..Options::default()
        },
    );
    assert_eq!((by_size.totals.allocs, by_size.totals.live_bytes), (1, 0));

    let by_text = run(
        &t,
        &Options {
            filter: Filter {
                text: Some("SITE_B".into()),
                ..Filter::default()
            },
            ..Options::default()
        },
    );
    assert_eq!((by_text.totals.allocs, by_text.totals.live_bytes), (1, 20));

    let by_module = run(
        &t,
        &Options {
            filter: Filter {
                modules: vec!["nothing.dll".into()],
                ..Filter::default()
            },
            ..Options::default()
        },
    );
    assert_eq!(by_module.totals.allocs, 0);
}

#[test]
fn sampled_and_attached_captures_say_so() {
    let mut h = header();
    h.capture_mode = CaptureMode::Sampled { one_in: 10 };
    h.attached = true;
    let t = build(&h, &[alloc(100, 1, 0x1000, 8, 1)]);
    let a = run(&t, &Options::default());
    assert_eq!(a.scale(), 10);
    assert_eq!(a.capture.mode(), "attached");
    assert!(a.capture.is_estimate());
    assert!(a.warnings.iter().any(|w| w.contains("sampled")));
    assert!(a.warnings.iter().any(|w| w.contains("attached")));
}

#[test]
fn a_trace_without_events_analyzes_to_zeros() {
    let a = run(&build(&header(), &[]), &Options::default());
    assert_eq!(a.totals.allocs, 0);
    assert_eq!(a.peak.live_bytes, 0);
    assert!(a.timeline.is_empty() && a.churn.is_empty());
}

#[test]
fn inclusive_stats_sum_the_subtree() {
    let t = build(
        &header(),
        &[
            alloc(100, 1, 0x1000, 10, 1),
            alloc(110, 1, 0x2000, 20, 2),
            alloc(120, 1, 0x3000, 30, 3),
        ],
    );
    let a = run(&t, &Options::default());
    let inc = a.tree.inclusive(&a.stats);
    let main = (1..a.tree.len() as NodeId)
        .find(|&i| a.tree.node(i).frame.name == "main")
        .unwrap();
    assert_eq!(inc[main as usize].alloc_bytes, 60);
    assert_eq!(inc[0].alloc_bytes, 60);
}

#[test]
fn a_trace_cut_off_mid_chunk_still_yields_the_events_before_it() {
    let mut w = TraceWriter::new(Vec::new(), &header()).unwrap();
    for s in stacks() {
        w.intern_stack(&s);
    }
    w.write_event(&alloc(100, 1, 0x1000, 8, 1)).unwrap();
    w.flush().unwrap();
    w.write_event(&alloc(200, 1, 0x2000, 8, 1)).unwrap();
    let mut bytes = w.finish(Dropped::default()).unwrap();
    bytes.truncate(bytes.len() - 20); // into the second events chunk or the footer
    let t = Trace::read(&bytes[..]).unwrap();
    let a = run(&t, &Options::default());
    assert!(a.totals.allocs >= 1);
    assert!(!a.warnings.is_empty());
}

#[test]
fn sizes_that_sum_past_u64_max_saturate_and_are_flagged() {
    let t = build(
        &header(),
        &[
            alloc(100, 1, 0x1000, u64::MAX - 5, 1),
            alloc(200, 1, 0x2000, u64::MAX - 5, 1),
            alloc(300, 1, 0x3000, 64, 2),
            free(400, 0x1000),
            free(500, 0x2000),
        ],
    );
    let a = run(&t, &Options::default());
    assert_eq!(a.totals.alloc_bytes, u64::MAX);
    assert_eq!(a.peak.live_bytes, u64::MAX);
    assert!(
        a.warnings.iter().any(|w| w.contains("exceed 2^64")),
        "{:?}",
        a.warnings
    );
    let plain = run(
        &build(&header(), &[alloc(1, 1, 0x1000, 64, 1)]),
        &Options::default(),
    );
    assert!(!plain.warnings.iter().any(|w| w.contains("exceed 2^64")));
}
