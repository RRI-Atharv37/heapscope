//! Shared helpers for the capture tests: locating the sample programs, running the CLI, and
//! replaying a trace into the live set and per-stack totals.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use heapscope_trace::{ChurnSummary, Event, Footer, Header, StackId, StackTable, TraceReader};

pub fn heapscope() -> Command {
    Command::new(env!("CARGO_BIN_EXE_heapscope"))
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

/// Directory holding the built sample programs, or `None` to skip the test.
/// Set `HEAPSCOPE_REQUIRE_SAMPLES=1` (CI does) to turn "missing" into a failure.
pub fn samples_dir() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(d) = std::env::var_os("HEAPSCOPE_SAMPLES") {
        candidates.push(PathBuf::from(d));
    }
    let root = workspace_root();
    candidates.push(root.join("build/samples-RelWithDebInfo"));
    candidates.push(root.join("build/samples/RelWithDebInfo"));
    for c in &candidates {
        if c.join("leak.exe").is_file() {
            return Some(c.clone());
        }
    }
    assert!(
        std::env::var_os("HEAPSCOPE_REQUIRE_SAMPLES").is_none(),
        "sample programs not found (looked in {candidates:?}); build them with cmake first"
    );
    eprintln!("skipped: sample programs are not built (see README)");
    None
}

pub fn sample(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.exe"))
}

/// A fresh scratch directory for this test binary.
pub fn scratch(test: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join(format!("heapscope-tests-{}", std::process::id()))
        .join(test);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

pub fn stdout_text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// One site from a sample's `--expected` file.
#[derive(Debug)]
pub struct ExpectedSite {
    pub function: String,
    pub allocs: u64,
    pub alloc_bytes: u64,
    pub live_allocs: u64,
    pub live_bytes: u64,
}

pub fn read_expected(path: &Path) -> Vec<ExpectedSite> {
    fn number(line: &str, key: &str) -> u64 {
        let pat = format!("\"{key}\": ");
        let at = line
            .find(&pat)
            .unwrap_or_else(|| panic!("no {key} in {line}"))
            + pat.len();
        line[at..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>()
            .parse()
            .unwrap()
    }
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    text.lines()
        .filter(|l| l.contains("\"function\""))
        .map(|l| {
            let f = l.find("\"function\": \"").unwrap() + 13;
            ExpectedSite {
                function: l[f..].split('"').next().unwrap().to_string(),
                allocs: number(l, "allocs"),
                alloc_bytes: number(l, "alloc_bytes"),
                live_allocs: number(l, "live_allocs_at_exit"),
                live_bytes: number(l, "live_bytes_at_exit"),
            }
        })
        .collect()
}

#[derive(Clone, Copy, Debug)]
pub struct Block {
    pub size: u64,
    pub stack: StackId,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Totals {
    pub allocs: u64,
    pub bytes: u64,
}

pub struct Replay {
    pub header: Header,
    pub footer: Option<Footer>,
    pub file_bytes: u64,
    pub stacks: StackTable,
    /// All events, ordered by timestamp.
    pub events: Vec<Event>,
    /// Blocks still allocated at the end of the trace.
    pub live: HashMap<u64, Block>,
    /// Allocations ever recorded, per stack.
    pub allocated: HashMap<StackId, Totals>,
    pub churn: HashMap<StackId, ChurnSummary>,
    pub markers: Vec<(u64, String)>,
    pub modules: Vec<(u64, u64, String)>,
    pub frees_without_alloc: u64,
}

fn ts_of(e: &Event) -> u64 {
    match e {
        Event::Alloc { ts_ns, .. }
        | Event::Free { ts_ns, .. }
        | Event::Realloc { ts_ns, .. }
        | Event::Marker { ts_ns, .. }
        | Event::ModuleLoad { ts_ns, .. }
        | Event::ModuleUnload { ts_ns, .. }
        | Event::ThreadName { ts_ns, .. }
        | Event::ChurnSummary { ts_ns, .. }
        | Event::HandleOpen { ts_ns, .. }
        | Event::HandleClose { ts_ns, .. } => *ts_ns,
    }
}

pub fn replay(path: &Path) -> Replay {
    let file = std::fs::File::open(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let file_bytes = file.metadata().unwrap().len();
    let mut reader = TraceReader::new(std::io::BufReader::new(file)).expect("trace header");
    let mut events: Vec<Event> = Vec::new();
    while let Some(e) = reader.next_event().expect("trace is readable to the end") {
        events.push(e);
    }
    events.sort_by_key(ts_of); // stable: equal timestamps keep file order
    let mut r = Replay {
        header: reader.header().clone(),
        footer: reader.footer().copied(),
        file_bytes,
        stacks: reader.stacks().clone(),
        events: Vec::new(),
        live: HashMap::new(),
        allocated: HashMap::new(),
        churn: HashMap::new(),
        markers: Vec::new(),
        modules: Vec::new(),
        frees_without_alloc: 0,
    };
    for e in &events {
        match e {
            Event::Alloc {
                addr, size, stack, ..
            } => {
                r.live.insert(
                    *addr,
                    Block {
                        size: *size,
                        stack: *stack,
                    },
                );
                let t = r.allocated.entry(*stack).or_default();
                t.allocs += 1;
                t.bytes += size;
            }
            Event::Free { addr, .. } => {
                if r.live.remove(addr).is_none() {
                    r.frees_without_alloc += 1;
                }
            }
            Event::Realloc {
                old_addr,
                new_addr,
                new_size,
                stack,
                ..
            } => {
                if r.live.remove(old_addr).is_none() {
                    r.frees_without_alloc += 1;
                }
                r.live.insert(
                    *new_addr,
                    Block {
                        size: *new_size,
                        stack: *stack,
                    },
                );
            }
            Event::Marker { ts_ns, label, .. } => r.markers.push((*ts_ns, label.clone())),
            Event::ModuleLoad {
                base, size, path, ..
            } => r.modules.push((*base, *size, path.clone())),
            Event::ChurnSummary { summary, .. } => {
                let s = r.churn.entry(summary.stack).or_insert(ChurnSummary {
                    stack: summary.stack,
                    ..Default::default()
                });
                s.merge(summary);
            }
            _ => {}
        }
    }
    r.events = events;
    r
}

impl Replay {
    /// The stack that made the most allocations of exactly `size` bytes (and at least `min` of them).
    pub fn site_of_size(&self, size: u64, min: u64) -> Option<StackId> {
        let mut counts: HashMap<StackId, u64> = HashMap::new();
        for e in &self.events {
            if let Event::Alloc { size: s, stack, .. } = e {
                if *s == size {
                    *counts.entry(*stack).or_default() += 1;
                }
            }
        }
        counts
            .into_iter()
            .filter(|&(_, n)| n >= min)
            .max_by_key(|&(_, n)| n)
            .map(|(s, _)| s)
    }

    pub fn live_at(&self, stack: StackId) -> Totals {
        let mut t = Totals::default();
        for b in self.live.values().filter(|b| b.stack == stack) {
            t.allocs += 1;
            t.bytes += b.size;
        }
        t
    }

    pub fn count_allocs_of_size(&self, size: u64) -> u64 {
        self.events
            .iter()
            .filter(|e| matches!(e, Event::Alloc { size: s, .. } if *s == size))
            .count() as u64
    }

    /// Does any frame of `stack` lie inside the module whose path ends with `file_name`?
    pub fn stack_reaches_module(&self, stack: StackId, file_name: &str) -> bool {
        let Some((base, size, _)) = self.modules.iter().find(|(_, _, p)| {
            p.to_ascii_lowercase()
                .ends_with(&file_name.to_ascii_lowercase())
        }) else {
            return false;
        };
        self.stacks
            .get(stack)
            .is_some_and(|frames| frames.iter().any(|&f| f >= *base && f < base + size))
    }
}

/// Run `heapscope run` on a sample and replay the resulting trace. `capture_args` go before `--`.
pub fn run_sample(
    dir: &Path,
    name: &str,
    test: &str,
    capture_args: &[&str],
    sample_args: &[&str],
) -> (Replay, PathBuf, Output) {
    let work = scratch(test);
    let trace = work.join(format!("{name}.hstrace"));
    let expected = work.join(format!("{name}.expected.json"));
    let mut cmd = heapscope();
    cmd.arg("run")
        .arg("-o")
        .arg(&trace)
        .args(capture_args)
        .arg("--")
        .arg(sample(dir, name))
        .args(sample_args)
        .arg("--expected")
        .arg(&expected);
    let out = cmd.output().unwrap_or_else(|e| {
        panic!("cannot start heapscope ({e}); on a PC with Smart App Control a fresh unsigned build may be blocked, see .project/spikes/capture-spike/rebuild.ps1")
    });
    assert!(
        out.status.success(),
        "heapscope run failed: {}",
        stdout_text(&out)
    );
    (replay(&trace), expected, out)
}

/// Assert that each expected site shows up in the trace with the right totals and live set.
pub fn assert_matches_expected(r: &Replay, expected: &[ExpectedSite]) {
    for site in expected {
        let candidates: Vec<StackId> = r
            .allocated
            .iter()
            .filter(|(_, t)| t.allocs == site.allocs && t.bytes == site.alloc_bytes)
            .map(|(s, _)| *s)
            .collect();
        assert!(
            !candidates.is_empty(),
            "no stack with {} allocs / {} bytes for {}",
            site.allocs,
            site.alloc_bytes,
            site.function
        );
        let ok = candidates.iter().any(|&s| {
            let live = r.live_at(s);
            live.allocs == site.live_allocs && live.bytes == site.live_bytes
        });
        assert!(
            ok,
            "{}: live set at exit differs from expected ({} blocks, {} bytes)",
            site.function, site.live_allocs, site.live_bytes
        );
    }
}
