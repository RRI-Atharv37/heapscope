use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use heapscope_trace::{
    AllocKind, CaptureMode, ChurnSummary, Dropped, Error, Event, HandleKind, Header, StackId,
    TraceReader, TraceWriter, FORMAT_MAJOR, NO_STACK,
};

fn header() -> Header {
    Header {
        tool_version: "0.1.0".into(),
        pid: 4242,
        exe_path: r"C:\app\app.exe".into(),
        command_line: "app.exe --flag".into(),
        pointer_size: 8,
        start_unix_ns: 1_760_000_000_000_000_000,
        capture_mode: CaptureMode::Full,
        min_lifetime_ms: 0,
        attached: false,
    }
}

/// Deterministic xorshift so the tests need no RNG crate.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

struct Written {
    bytes: Vec<u8>,
    events: Vec<Event>,
    stacks: Vec<Vec<u64>>,
}

/// Write `events` (stacks given by index into `stacks`) and keep what was written.
fn write_trace(
    header: &Header,
    stacks: &[Vec<u64>],
    events: &[Event],
    dropped: Dropped,
) -> Written {
    let mut w = TraceWriter::new(Vec::new(), header).unwrap();
    for (i, s) in stacks.iter().enumerate() {
        assert_eq!(w.intern_stack(s), (i + 1) as StackId);
    }
    for e in events {
        w.write_event(e).unwrap();
    }
    let bytes = w.finish(dropped).unwrap();
    Written {
        bytes,
        events: events.to_vec(),
        stacks: stacks.to_vec(),
    }
}

fn read_all(bytes: &[u8]) -> (TraceReader<&[u8]>, Vec<Event>) {
    let mut r = TraceReader::new(bytes).unwrap();
    let events = r.by_ref().collect::<Result<Vec<_>, _>>().unwrap();
    (r, events)
}

fn one_of_each() -> (Vec<Vec<u64>>, Vec<Event>) {
    let stacks = vec![
        vec![0x7ff6_0000_1000, 0x7ff6_0000_2000, 0x7ffa_1234_0000],
        vec![0x10],
    ];
    let events = vec![
        Event::ModuleLoad {
            ts_ns: 0,
            thread: 1,
            base: 0x7ff6_0000_0000,
            size: 0x20000,
            pdb_guid: [7; 16],
            pdb_age: 3,
            path: r"C:\app\app.exe".into(),
        },
        Event::ThreadName {
            ts_ns: 5,
            thread: 2,
            name: "worker".into(),
        },
        Event::Alloc {
            ts_ns: 10,
            thread: 1,
            kind: AllocKind::Heap,
            addr: 0x1f00_0000_1000,
            size: 64,
            requested: 52,
            stack: 1,
        },
        Event::Alloc {
            ts_ns: 12,
            thread: 2,
            kind: AllocKind::Pool,
            addr: 0x1f00_0000_0800,
            size: 128,
            requested: 128,
            stack: 2,
        },
        Event::Alloc {
            ts_ns: 13,
            thread: 2,
            kind: AllocKind::Virtual,
            addr: 0x2000_0000_0000,
            size: 1 << 20,
            requested: 1 << 20,
            stack: NO_STACK,
        },
        Event::Realloc {
            ts_ns: 20,
            thread: 1,
            old_addr: 0x1f00_0000_1000,
            new_addr: 0x1f00_0000_3000,
            old_size: 64,
            new_size: 256,
            requested: 250,
            stack: 1,
        },
        Event::Marker {
            ts_ns: 25,
            thread: 1,
            label: "frame 1 — grüße".into(),
        },
        Event::Free {
            ts_ns: 30,
            thread: 2,
            addr: 0x1f00_0000_0800,
            stack: 2,
        },
        Event::ChurnSummary {
            ts_ns: 31,
            summary: ChurnSummary {
                stack: 1,
                allocs: 10,
                bytes: 640,
                total_lifetime_ns: 12_345,
                max_concurrent: 4,
            },
        },
        Event::HandleOpen {
            ts_ns: 32,
            thread: 1,
            handle: 0x1c4,
            kind: HandleKind::Mutant,
            stack: 1,
        },
        Event::HandleOpen {
            ts_ns: 33,
            thread: 1,
            handle: 0x1c8,
            kind: HandleKind::Other(99),
            stack: 2,
        },
        Event::HandleClose {
            ts_ns: 34,
            thread: 1,
            handle: 0x1c4,
            stack: 1,
        },
        Event::ModuleUnload {
            ts_ns: 40,
            thread: 1,
            base: 0x7ff6_0000_0000,
        },
        // Out-of-order timestamps and addresses must survive (per-thread buffers merge late).
        Event::Free {
            ts_ns: 35,
            thread: 3,
            addr: 0x10,
            stack: NO_STACK,
        },
        Event::Free {
            ts_ns: u64::MAX,
            thread: u32::MAX,
            addr: u64::MAX,
            stack: 2,
        },
    ];
    (stacks, events)
}

#[test]
fn every_event_kind_roundtrips() {
    let (stacks, events) = one_of_each();
    let dropped = Dropped {
        overflow: 3,
        min_lifetime: 10,
        sampling: 0,
    };
    let written = write_trace(&header(), &stacks, &events, dropped);

    let (r, read) = read_all(&written.bytes);
    assert_eq!(read, written.events);
    assert_eq!(r.header(), &header());
    assert_eq!(r.version(), (FORMAT_MAJOR, heapscope_trace::FORMAT_MINOR));
    assert_eq!(r.stacks().len(), 2);
    assert_eq!(r.stacks().get(1).unwrap(), written.stacks[0].as_slice());
    assert_eq!(r.stacks().get(2).unwrap(), written.stacks[1].as_slice());
    assert!(r.stacks().get(NO_STACK).is_none());
    assert!(r.stacks().get(3).is_none());
    let footer = r.footer().expect("clean shutdown writes a footer");
    assert_eq!(footer.event_count, events.len() as u64);
    assert_eq!(footer.stack_count, 2);
    assert_eq!(footer.dropped, dropped);
}

#[test]
fn identical_stacks_are_deduplicated() {
    let mut w = TraceWriter::new(Vec::new(), &header()).unwrap();
    let a = w.intern_stack(&[1, 2, 3]);
    let b = w.intern_stack(&[1, 2, 3]);
    let c = w.intern_stack(&[1, 2]);
    assert_eq!(a, b);
    assert_ne!(a, c);
    assert_eq!(w.intern_stack(&[]), NO_STACK);
}

#[test]
fn sampled_header_roundtrips_and_is_partial() {
    let mut h = header();
    h.capture_mode = CaptureMode::Sampled { one_in: 16 };
    let written = write_trace(
        &h,
        &[],
        &[],
        Dropped {
            sampling: 1500,
            ..Dropped::default()
        },
    );
    let (r, events) = read_all(&written.bytes);
    assert!(events.is_empty());
    assert_eq!(r.header().capture_mode, CaptureMode::Sampled { one_in: 16 });
    assert!(r.header().is_partial());
    assert_eq!(r.footer().unwrap().dropped.sampling, 1500);
    assert!(!header().is_partial());
}

#[test]
fn attached_header_roundtrips_and_is_partial() {
    let mut h = header();
    h.attached = true;
    let written = write_trace(&h, &[], &[], Dropped::default());
    let (r, _) = read_all(&written.bytes);
    assert!(r.header().attached);
    assert!(r.header().is_partial());
}

#[test]
fn events_spanning_many_chunks_roundtrip() {
    let stacks: Vec<Vec<u64>> = (0..300u64)
        .map(|i| vec![0x1000 + i, 0x2000 + i * 3])
        .collect();
    let mut rng = Rng(0x1234_5678_9abc_def1);
    let mut events = Vec::new();
    for i in 0..60_000u64 {
        events.push(Event::Alloc {
            ts_ns: i * 100,
            thread: rng.below(8) as u32,
            kind: AllocKind::Heap,
            addr: 0x1f00_0000_0000 + rng.below(1 << 24) * 16,
            size: 16 + rng.below(4096),
            requested: 16,
            stack: 1 + rng.below(300) as StackId,
        });
    }
    let written = write_trace(&header(), &stacks, &events, Dropped::default());
    let (r, read) = read_all(&written.bytes);
    assert_eq!(read, events);
    assert_eq!(r.footer().unwrap().event_count, 60_000);
}

#[test]
fn bad_magic_is_rejected() {
    let err = TraceReader::new(&b"NOTATRACEFILE"[..]).err().unwrap();
    assert!(matches!(err, Error::BadMagic), "{err}");
    let err = TraceReader::new(&b""[..]).err().unwrap();
    assert!(matches!(err, Error::BadMagic), "{err}");
}

#[test]
fn newer_major_version_is_rejected_and_newer_minor_accepted() {
    let written = write_trace(&header(), &[], &[], Dropped::default());

    let mut newer_major = written.bytes.clone();
    newer_major[8..10].copy_from_slice(&(FORMAT_MAJOR + 1).to_le_bytes());
    let err = TraceReader::new(newer_major.as_slice()).err().unwrap();
    assert!(
        matches!(err, Error::UnsupportedVersion { found, supported }
            if found == FORMAT_MAJOR + 1 && supported == FORMAT_MAJOR),
        "{err}"
    );

    let mut newer_minor = written.bytes.clone();
    newer_minor[10..12].copy_from_slice(&7u16.to_le_bytes());
    let r = TraceReader::new(newer_minor.as_slice()).unwrap();
    assert_eq!(r.version(), (FORMAT_MAJOR, 7));
}

#[test]
fn flipped_byte_fails_the_checksum() {
    let (stacks, events) = one_of_each();
    let written = write_trace(&header(), &stacks, &events, Dropped::default());
    let mut bytes = written.bytes;
    // Inside the first events chunk, well past the header chunk.
    let at = bytes.len() / 2;
    bytes[at] ^= 0x40;
    let mut r = TraceReader::new(bytes.as_slice()).unwrap();
    let err = r
        .by_ref()
        .find_map(Result::err)
        .expect("corruption must be reported");
    assert!(
        matches!(
            err,
            Error::ChecksumMismatch | Error::Corrupt(_) | Error::Truncated
        ),
        "{err}"
    );
}

#[test]
fn killed_capture_leaves_a_readable_prefix() {
    // Many chunks, so cutting at a chunk boundary is possible.
    let mut w = TraceWriter::new(Vec::new(), &header()).unwrap();
    let stack = w.intern_stack(&[0x100, 0x200]);
    let mut sizes = Vec::new();
    for i in 0..40_000u64 {
        w.write_event(&Event::Alloc {
            ts_ns: i,
            thread: 1,
            kind: AllocKind::Heap,
            addr: 0x1000 + i * 64,
            size: 64,
            requested: 64,
            stack,
        })
        .unwrap();
        if sizes.last() != Some(&w.bytes_written()) {
            sizes.push(w.bytes_written());
        }
    }
    let full = w.finish(Dropped::default()).unwrap();
    // `sizes` holds the file offset after every flushed chunk: cut exactly at the second.
    let boundary = sizes[sizes.len() / 2] as usize;
    let cut = &full[..boundary];
    let mut r = TraceReader::new(cut).unwrap();
    let mut n = 0;
    while let Some(_event) = r.next_event().unwrap() {
        n += 1;
    }
    assert!(n > 0 && n < 40_000);
    assert!(
        r.footer().is_none(),
        "no footer means the capture did not shut down cleanly"
    );

    // Cutting through a chunk is reported as truncation, after the intact events.
    let mid = &full[..boundary - 5];
    let mut r = TraceReader::new(mid).unwrap();
    let outcome: Result<Vec<_>, _> = r.by_ref().collect();
    assert!(matches!(outcome, Err(Error::Truncated)));
}

#[test]
fn footer_count_mismatch_is_detected() {
    let (stacks, events) = one_of_each();
    let written = write_trace(&header(), &stacks, &events, Dropped::default());
    // Drop the events chunk's worth of data by re-writing with fewer events but keeping the
    // footer of the original: simplest is to splice two traces together.
    let short = write_trace(&header(), &stacks, &events[..2], Dropped::default());
    let footer_len = 14 + 5; // frame + five one-byte varints (all counts < 128)
    let mut franken = short.bytes[..short.bytes.len() - footer_len].to_vec();
    franken.extend_from_slice(&written.bytes[written.bytes.len() - footer_len..]);
    let mut r = TraceReader::new(franken.as_slice()).unwrap();
    let outcome: Result<Vec<_>, _> = r.by_ref().collect();
    assert!(matches!(outcome, Err(Error::Corrupt(_))), "{outcome:?}");
}

/// Format 1.1: a free of a pool object is its own event, so it never matches a heap block.
#[test]
fn pool_free_roundtrips_next_to_a_heap_free_at_the_same_address() {
    let (stacks, _) = one_of_each();
    let events = vec![
        Event::PoolFree {
            ts_ns: 30,
            thread: 2,
            addr: 0x1f00_0000_0800,
            stack: NO_STACK,
        },
        Event::Free {
            ts_ns: 31,
            thread: 2,
            addr: 0x1f00_0000_0800,
            stack: NO_STACK,
        },
    ];
    let written = write_trace(&header(), &stacks, &events, Dropped::default());
    let (_, read) = read_all(&written.bytes);
    assert_eq!(read, events);
}

#[test]
fn committed_fixture_still_reads() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tiny.hstrace");
    let (stacks, events) = one_of_each();
    let dropped = Dropped {
        overflow: 0,
        min_lifetime: 10,
        sampling: 0,
    };
    let mut h = header();
    h.min_lifetime_ms = 1;
    if std::env::var_os("HEAPSCOPE_REGEN_FIXTURE").is_some() {
        let written = write_trace(&h, &stacks, &events, dropped);
        std::fs::write(&path, written.bytes).unwrap();
    }
    let bytes =
        std::fs::read(&path).expect("fixture missing; regenerate with HEAPSCOPE_REGEN_FIXTURE=1");
    let (r, read) = read_all(&bytes);
    assert_eq!(read, events);
    assert_eq!(r.header(), &h);
    assert_eq!(r.footer().unwrap().dropped, dropped);
    assert!(
        bytes.len() < 1024,
        "fixture should stay tiny, is {} bytes",
        bytes.len()
    );
}

// ---- Size on 1M synthetic events -------------------------------------------------------

/// A plausible allocation workload: 500k allocations, 500k frees, a few thousand stacks,
/// mostly small sizes, addresses clustered in a few regions, short and long lifetimes.
fn synthetic_million() -> (Vec<Vec<u64>>, Vec<Event>) {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let stacks: Vec<Vec<u64>> = (0..3000u64)
        .map(|s| {
            let depth = 6 + rng.below(10);
            (0..depth)
                .map(|d| 0x7ff6_1000_0000 + (s * 97 + d * 4093) * 16 + rng.below(8))
                .collect()
        })
        .collect();
    let sizes = [16u64, 24, 32, 48, 64, 96, 128, 256, 512, 1024, 4096];
    let mut events = Vec::with_capacity(1_000_000);
    let mut live: Vec<(u64, StackId)> = Vec::new();
    let mut next_addr = 0x1f00_0000_0000u64;
    let mut ts = 0u64;
    while events.len() < 1_000_000 {
        ts += 50 + rng.below(2000);
        let thread = 1 + rng.below(4) as u32;
        let free_now = !live.is_empty() && (live.len() > 50_000 || rng.below(2) == 0);
        if free_now {
            let i = rng.below(live.len() as u64) as usize;
            let (addr, stack) = live.swap_remove(i);
            events.push(Event::Free {
                ts_ns: ts,
                thread,
                addr,
                stack,
            });
        } else {
            let size = sizes[rng.below(sizes.len() as u64) as usize];
            let stack = 1 + rng.below(stacks.len() as u64) as StackId;
            // Heap addresses mostly advance, with some reuse jumping back.
            if rng.below(4) == 0 {
                next_addr = 0x1f00_0000_0000 + rng.below(1 << 28) * 16;
            }
            next_addr += size.next_multiple_of(16) + 16;
            events.push(Event::Alloc {
                ts_ns: ts,
                thread,
                kind: AllocKind::Heap,
                addr: next_addr,
                size,
                requested: size,
                stack,
            });
            live.push((next_addr, stack));
        }
    }
    (stacks, events)
}

#[test]
fn one_million_events_roundtrip_and_size() {
    let (stacks, events) = synthetic_million();
    let written = write_trace(&header(), &stacks, &events, Dropped::default());
    let per_event = written.bytes.len() as f64 / events.len() as f64;
    println!(
        "1M events: {} bytes on disk, {per_event:.2} B/event ({} stacks)",
        written.bytes.len(),
        stacks.len()
    );
    // Uncompressed fixed-size records (the spike) take 56 B/event; stay far below that.
    assert!(per_event < 13.0, "trace is {per_event:.2} B/event");

    let (r, read) = read_all(&written.bytes);
    assert_eq!(read.len(), 1_000_000);
    assert!(read == events, "events differ after the round trip");
    assert_eq!(r.stacks().len(), stacks.len());
}

// ---- min_lifetime ----------------------------------------------------------------------

struct Lifetime {
    alloc_ts: u64,
    free_ts: u64,
    thread: u32,
    addr: u64,
    size: u64,
    stack: StackId,
}

/// The reduction the capture performs for `--min-lifetime`, done offline on a full stream:
/// allocations freed within `min_ns` become neither event, only per-stack totals.
fn reduce(lifetimes: &[Lifetime], min_ns: u64) -> (Vec<Event>, BTreeMap<StackId, ChurnSummary>) {
    let mut events = Vec::new();
    let mut summaries: BTreeMap<StackId, ChurnSummary> = BTreeMap::new();
    let mut edges: HashMap<StackId, Vec<(u64, i64)>> = HashMap::new();
    for l in lifetimes {
        let age = l.free_ts - l.alloc_ts;
        if age < min_ns {
            let s = summaries.entry(l.stack).or_insert(ChurnSummary {
                stack: l.stack,
                ..Default::default()
            });
            s.record(l.size, age);
            let e = edges.entry(l.stack).or_default();
            e.push((l.alloc_ts, 1));
            e.push((l.free_ts, -1));
        } else {
            events.push(Event::Alloc {
                ts_ns: l.alloc_ts,
                thread: l.thread,
                kind: AllocKind::Heap,
                addr: l.addr,
                size: l.size,
                requested: l.size,
                stack: l.stack,
            });
            events.push(Event::Free {
                ts_ns: l.free_ts,
                thread: l.thread,
                addr: l.addr,
                stack: l.stack,
            });
        }
    }
    for (stack, mut e) in edges {
        // Frees sort before allocs at the same instant.
        e.sort();
        let (mut cur, mut max) = (0i64, 0i64);
        for (_, d) in e {
            cur += d;
            max = max.max(cur);
        }
        summaries.get_mut(&stack).unwrap().max_concurrent = max as u64;
    }
    events.sort_by_key(|e| match e {
        Event::Alloc { ts_ns, .. } | Event::Free { ts_ns, .. } => *ts_ns,
        _ => 0,
    });
    (events, summaries)
}

fn full_events(lifetimes: &[Lifetime]) -> Vec<Event> {
    let mut events = Vec::new();
    for l in lifetimes {
        events.push(Event::Alloc {
            ts_ns: l.alloc_ts,
            thread: l.thread,
            kind: AllocKind::Heap,
            addr: l.addr,
            size: l.size,
            requested: l.size,
            stack: l.stack,
        });
        events.push(Event::Free {
            ts_ns: l.free_ts,
            thread: l.thread,
            addr: l.addr,
            stack: l.stack,
        });
    }
    events.sort_by_key(|e| match e {
        Event::Alloc { ts_ns, .. } | Event::Free { ts_ns, .. } => *ts_ns,
        _ => 0,
    });
    events
}

#[test]
fn min_lifetime_trace_is_smaller_and_churn_totals_are_exact() {
    let mut rng = Rng(0xfeed_beef_1234_5678);
    let stacks: Vec<Vec<u64>> = (0..40u64)
        .map(|s| {
            (0..8)
                .map(|d| 0x7ff6_2000_0000 + s * 512 + d * 32)
                .collect()
        })
        .collect();
    let min_ns = 1_000_000; // 1 ms
    let mut lifetimes = Vec::new();
    let mut ts = 0u64;
    let mut addr = 0x1f00_0000_0000u64;
    for i in 0..200_000u64 {
        ts += 200 + rng.below(800);
        addr += 64;
        // 95 % churn (microsecond lifetimes), 5 % long-lived.
        let age = if i % 20 == 0 {
            2_000_000 + rng.below(50_000_000)
        } else {
            1_000 + rng.below(900_000)
        };
        lifetimes.push(Lifetime {
            alloc_ts: ts,
            free_ts: ts + age,
            thread: 1 + rng.below(3) as u32,
            addr,
            size: 32 + rng.below(8) * 16,
            // Skewed so a few stacks dominate.
            stack: 1 + (rng.below(40) * rng.below(40) / 40) as StackId,
        });
    }

    let full = full_events(&lifetimes);
    let full_trace = write_trace(&header(), &stacks, &full, Dropped::default());

    let (kept, summaries) = reduce(&lifetimes, min_ns);
    let dropped_pairs: u64 = summaries.values().map(|s| s.allocs).sum();
    assert_eq!(dropped_pairs, 190_000);
    let mut h = header();
    h.min_lifetime_ms = 1;
    let mut reduced_events = kept.clone();
    let end_ts = reduced_events.last().map_or(0, |e| match e {
        Event::Alloc { ts_ns, .. } | Event::Free { ts_ns, .. } => *ts_ns,
        _ => 0,
    });
    reduced_events.extend(summaries.values().map(|s| Event::ChurnSummary {
        ts_ns: end_ts,
        summary: *s,
    }));
    let reduced_trace = write_trace(
        &h,
        &stacks,
        &reduced_events,
        Dropped {
            min_lifetime: dropped_pairs,
            ..Dropped::default()
        },
    );

    println!(
        "min_lifetime: full {} B ({} events), reduced {} B ({} events, {} churn summaries)",
        full_trace.bytes.len(),
        full.len(),
        reduced_trace.bytes.len(),
        reduced_events.len(),
        summaries.len()
    );
    assert!(
        reduced_trace.bytes.len() * 5 < full_trace.bytes.len(),
        "reduced trace is not measurably smaller"
    );

    // Read the reduced trace back and check the totals against the events that were dropped.
    let (r, read) = read_all(&reduced_trace.bytes);
    assert_eq!(r.header().min_lifetime_ms, 1);
    assert_eq!(r.footer().unwrap().dropped.min_lifetime, dropped_pairs);
    let read_summaries: BTreeMap<StackId, ChurnSummary> = read
        .iter()
        .filter_map(|e| match e {
            Event::ChurnSummary { summary, .. } => Some((summary.stack, *summary)),
            _ => None,
        })
        .collect();
    assert_eq!(read_summaries, summaries);

    let dropped: Vec<&Lifetime> = lifetimes
        .iter()
        .filter(|l| l.free_ts - l.alloc_ts < min_ns)
        .collect();
    assert_eq!(
        read_summaries.values().map(|s| s.allocs).sum::<u64>(),
        dropped.len() as u64
    );
    assert_eq!(
        read_summaries.values().map(|s| s.bytes).sum::<u64>(),
        dropped.iter().map(|l| l.size).sum::<u64>()
    );
    assert_eq!(
        read_summaries
            .values()
            .map(|s| s.total_lifetime_ns)
            .sum::<u64>(),
        dropped.iter().map(|l| l.free_ts - l.alloc_ts).sum::<u64>()
    );
    // Kept allocations are still all there, and no dropped allocation leaked into the events.
    let kept_allocs = read
        .iter()
        .filter(|e| matches!(e, Event::Alloc { .. }))
        .count();
    assert_eq!(kept_allocs, lifetimes.len() - dropped.len());
}

#[test]
fn churn_summary_merge_adds_totals_and_keeps_peak() {
    let mut a = ChurnSummary {
        stack: 1,
        allocs: 2,
        bytes: 10,
        total_lifetime_ns: 100,
        max_concurrent: 3,
    };
    let b = ChurnSummary {
        stack: 1,
        allocs: 5,
        bytes: 50,
        total_lifetime_ns: 400,
        max_concurrent: 2,
    };
    a.merge(&b);
    assert_eq!(
        a,
        ChurnSummary {
            stack: 1,
            allocs: 7,
            bytes: 60,
            total_lifetime_ns: 500,
            max_concurrent: 3
        }
    );
}
