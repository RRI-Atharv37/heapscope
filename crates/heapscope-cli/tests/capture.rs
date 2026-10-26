//! End-to-end capture tests (S1.4): the real CLI injects the real DLL into the sample programs
//! and the resulting traces are checked against what each sample says it did.

mod support;

use std::io::{BufRead, BufReader, Write};
use std::process::Stdio;

use heapscope_trace::CaptureMode;
use support::*;

#[test]
fn run_leak_records_the_known_leaks_and_nothing_else() {
    let Some(dir) = samples_dir() else { return };
    let (r, expected_path, out) = run_sample(&dir, "leak", "leak", &[], &[]);
    let expected = read_expected(&expected_path);
    assert_matches_expected(&r, &expected);

    // The two leaks are live at exit, grouped under their own call sites, and both stacks reach leak.exe.
    let a = r.site_of_size(1024, 100).expect("leak_site_a stack");
    let b = r.site_of_size(65536, 10).expect("leak_site_b stack");
    assert_ne!(a, b);
    assert_eq!((r.live_at(a).allocs, r.live_at(a).bytes), (100, 102_400));
    assert_eq!((r.live_at(b).allocs, r.live_at(b).bytes), (10, 655_360));
    assert!(
        r.stack_reaches_module(a, "leak.exe"),
        "leak_site_a stack never reaches leak.exe: module log or stack capture is wrong"
    );
    assert!(r.stack_reaches_module(b, "leak.exe"));
    // balanced_site: 100 x 512 B allocated, every one freed.
    let balanced = r.site_of_size(512, 100).expect("balanced_site stack");
    assert_eq!(r.live_at(balanced).allocs, 0);

    assert!(r.footer.is_some(), "trace has no footer (unclean shutdown)");
    assert_eq!(r.footer.unwrap().dropped.overflow, 0);
    assert!(
        !r.header.attached && r.header.capture_mode == CaptureMode::Full && !r.header.is_partial()
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("events"),
        "summary line missing: {}",
        stdout_text(&out)
    );
}

#[test]
fn run_basic_alloc_matches_ground_truth_exactly() {
    let Some(dir) = samples_dir() else { return };
    let (r, expected_path, _) = run_sample(&dir, "basic-alloc", "basic", &[], &[]);
    assert_matches_expected(&r, &read_expected(&expected_path));
}

#[test]
fn run_multithread_keeps_cross_thread_frees_and_loses_nothing() {
    let Some(dir) = samples_dir() else { return };
    let (r, expected_path, _) = run_sample(&dir, "multithread", "mt", &[], &[]);
    assert_matches_expected(&r, &read_expected(&expected_path));
    assert_eq!(r.footer.unwrap().dropped.overflow, 0, "events were dropped");
    let handoff = r.site_of_size(128, 7_000).expect("handoff_site stack");
    let threads: std::collections::HashSet<u32> = r
        .events
        .iter()
        .filter_map(|e| match e {
            heapscope_trace::Event::Alloc { thread, stack, .. } if *stack == handoff => {
                Some(*thread)
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        threads.len(),
        8,
        "allocations should come from 8 distinct threads"
    );
}

#[test]
fn run_passes_the_exit_code_through_and_still_writes_a_trace() {
    let work = scratch("exit-code");
    let trace = work.join("cmd.hstrace");
    let out = heapscope()
        .arg("run")
        .arg("-o")
        .arg(&trace)
        .args(["--", "cmd.exe", "/c", "exit", "3"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3), "{}", stdout_text(&out));
    let r = replay(&trace);
    assert!(r.footer.is_some());
    assert!(!r.events.is_empty());
}

#[test]
fn run_reports_a_missing_program_and_a_bad_output_path() {
    let out = heapscope()
        .args(["run", "--", "definitely-not-a-program.exe"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stdout_text(&out).contains("cannot start"),
        "{}",
        stdout_text(&out)
    );

    let Some(dir) = samples_dir() else { return };
    let out = heapscope()
        .args(["run", "-o", "Z:\\no\\such\\dir\\x.hstrace", "--"])
        .arg(sample(&dir, "leak"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stdout_text(&out).contains("trace file could not be created"),
        "{}",
        stdout_text(&out)
    );
}

#[test]
fn sample_keeps_about_one_in_n_and_the_header_says_so() {
    let Some(dir) = samples_dir() else { return };
    let (r, _, _) = run_sample(
        &dir,
        "basic-alloc",
        "sample",
        &["--sample", "10"],
        &["10000"],
    );
    assert_eq!(r.header.capture_mode, CaptureMode::Sampled { one_in: 10 });
    assert!(r.header.is_partial());
    let site = r.site_of_size(32, 100).expect("small-block site");
    let kept = r.allocated[&site].allocs;
    let estimate = kept * 10;
    assert!(
        (8_500..=11_500).contains(&estimate),
        "kept {kept} of 10000, scaled estimate {estimate}"
    );
    assert_eq!(
        r.live_at(site).allocs,
        0,
        "a kept allocation must have its free kept too"
    );
    assert!(r.footer.unwrap().dropped.sampling > 8_000);
}

#[test]
fn min_lifetime_folds_churn_into_exact_totals_and_shrinks_the_trace() {
    let Some(dir) = samples_dir() else { return };
    let (full, _, _) = run_sample(&dir, "churn", "churn-full", &[], &[]);
    let (cut, expected_path, _) =
        run_sample(&dir, "churn", "churn-cut", &["--min-lifetime", "5ms"], &[]);

    assert_eq!(cut.header.min_lifetime_ms, 5);
    // The churn stack: the 48-byte allocations.
    let full_site = full
        .site_of_size(48, 40_000)
        .expect("churn stack in the full trace");
    assert_eq!(full.allocated[&full_site].allocs, 50_000);

    let summary = cut
        .churn
        .values()
        .max_by_key(|c| c.allocs)
        .expect("a ChurnSummary");
    assert_eq!(
        summary.bytes,
        summary.allocs * 48,
        "churn totals are for 48-byte blocks"
    );
    let kept = cut
        .allocated
        .get(&summary.stack)
        .copied()
        .unwrap_or_default();
    // Nothing is lost: folded + still-written = what the program did.
    assert_eq!(summary.allocs + kept.allocs, 50_000);
    assert_eq!(summary.bytes + kept.bytes, 50_000 * 48);
    assert!(
        summary.allocs >= 45_000,
        "only {} of 50000 short-lived allocations were folded",
        summary.allocs
    );
    assert!(
        summary.total_lifetime_ns > 0
            && summary.max_concurrent >= 1
            && summary.max_concurrent <= 1024
    );
    assert_eq!(
        cut.live_at(summary.stack).allocs,
        0,
        "no churn block may be left live"
    );
    // Every folded allocation in the process (this stack and any other) is in some summary.
    let folded_everywhere: u64 = cut.churn.values().map(|c| c.allocs).sum();
    assert_eq!(cut.footer.unwrap().dropped.min_lifetime, folded_everywhere);

    // The survivors are untouched, and the trace is much smaller.
    assert_matches_expected(
        &cut,
        &read_expected(&expected_path)
            .into_iter()
            .filter(|s| s.function == "survivor_site")
            .collect::<Vec<_>>(),
    );
    assert!(
        cut.file_bytes * 3 < full.file_bytes,
        "trace shrank only from {} to {} bytes",
        full.file_bytes,
        cut.file_bytes
    );
}

/// Line-oriented conversation with a child process.
struct Talker {
    child: std::process::Child,
    stdin: Option<std::process::ChildStdin>,
    stdout: BufReader<std::process::ChildStdout>,
}

impl Talker {
    fn spawn(mut cmd: std::process::Command) -> Self {
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn (Smart App Control may block fresh unsigned builds)");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            stdin: Some(stdin),
            stdout,
        }
    }

    fn read_line(&mut self) -> String {
        let mut s = String::new();
        let n = self.stdout.read_line(&mut s).unwrap();
        assert!(n > 0, "child closed stdout early");
        s.trim().to_string()
    }

    fn read_until(&mut self, needle: &str) -> String {
        for _ in 0..50 {
            let l = self.read_line();
            if l.contains(needle) {
                return l;
            }
        }
        panic!("never saw '{needle}'");
    }

    fn send(&mut self, line: &str) {
        let stdin = self.stdin.as_mut().expect("stdin already closed");
        writeln!(stdin, "{line}").unwrap();
        stdin.flush().unwrap();
    }

    fn close_stdin(&mut self) {
        self.stdin = None;
    }
}

#[test]
fn attach_captures_growth_between_markers_and_leaves_the_target_running() {
    let Some(dir) = samples_dir() else { return };
    let work = scratch("attach");
    let trace = work.join("growth-attach.hstrace");

    let mut growth = Talker::spawn({
        let mut c = std::process::Command::new(sample(&dir, "growth"));
        c.arg("--wait-for-attach");
        c
    });
    let pid: u32 = growth
        .read_line()
        .strip_prefix("PID ")
        .expect("PID line")
        .parse()
        .unwrap();

    let mut att = Talker::spawn({
        let mut c = heapscope();
        c.args(["attach", "--pid", &pid.to_string(), "--until-eof", "-o"])
            .arg(&trace);
        c
    });
    att.read_until("attached to process");

    att.send("start");
    att.read_until("marker recorded: start");
    growth.send("grow 16");
    assert_eq!(growth.read_line(), "grown 16");
    att.send("after-grow");
    att.read_until("marker recorded: after-grow");
    growth.send("grow 8");
    assert_eq!(growth.read_line(), "grown 8");
    growth.send("shrink 4");
    assert_eq!(growth.read_line(), "shrunk 4");
    att.send("end");
    att.read_until("marker recorded: end");

    // Closing stdin ends the capture; the CLI flushes, detaches and exits.
    att.close_stdin();
    let detached = att.read_until("detached from process");
    assert!(detached.contains("keeps running"));
    att.read_until("heapscope:");
    assert!(att.child.wait().unwrap().success());

    // The target is untouched: still running, still allocating (unrecorded now), exits normally.
    assert!(
        growth.child.try_wait().unwrap().is_none(),
        "target died during or after the attach"
    );
    growth.send("grow 2");
    assert_eq!(growth.read_line(), "grown 2");
    growth.send("quit");
    assert_eq!(growth.child.wait().unwrap().code(), Some(0));

    // The trace: attached, partial, closed cleanly, and the growth shows up between the markers.
    let r = replay(&trace);
    assert!(r.header.attached && r.header.is_partial());
    assert!(r.footer.is_some());
    assert_eq!(r.header.pid, pid);
    let labels: Vec<&str> = r.markers.iter().map(|(_, l)| l.as_str()).collect();
    assert_eq!(labels, ["start", "after-grow", "end"]);
    let (t0, t1, t2) = (r.markers[0].0, r.markers[1].0, r.markers[2].0);
    assert!(t0 < t1 && t1 < t2);
    let block = 65_536u64;
    // Frees carry no size, so a free counts when it releases a 64 KiB block allocated since the attach.
    let block_addrs: std::collections::HashSet<u64> = r
        .events
        .iter()
        .filter_map(|e| match e {
            heapscope_trace::Event::Alloc { addr, size, .. } if *size == block => Some(*addr),
            _ => None,
        })
        .collect();
    let net = |from: u64, to: u64| -> i64 {
        r.events
            .iter()
            .map(|e| match e {
                heapscope_trace::Event::Alloc { ts_ns, size, .. }
                    if *size == block && *ts_ns > from && *ts_ns < to =>
                {
                    1
                }
                heapscope_trace::Event::Free { ts_ns, addr, .. }
                    if *ts_ns > from && *ts_ns < to && block_addrs.contains(addr) =>
                {
                    -1
                }
                _ => 0,
            })
            .sum()
    };
    assert_eq!(net(t0, t1), 16, "growth between 'start' and 'after-grow'");
    assert_eq!(net(t1, t2), 8 - 4, "growth between 'after-grow' and 'end'");
}

#[test]
fn attach_refuses_a_missing_process_and_a_second_attach() {
    let out = heapscope()
        .args(["attach", "--pid", "4294967000", "--duration", "1s"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stdout_text(&out).contains("cannot open process"),
        "{}",
        stdout_text(&out)
    );

    let Some(dir) = samples_dir() else { return };
    let mut growth = Talker::spawn({
        let mut c = std::process::Command::new(sample(&dir, "growth"));
        c.arg("--wait-for-attach");
        c
    });
    let pid = growth.read_line().strip_prefix("PID ").unwrap().to_string();
    let work = scratch("attach-twice");
    let first = heapscope()
        .args(["attach", "--pid", &pid, "--duration", "1s", "-o"])
        .arg(work.join("a.hstrace"))
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(first.status.success(), "{}", stdout_text(&first));
    let second = heapscope()
        .args(["attach", "--pid", &pid, "--duration", "1s", "-o"])
        .arg(work.join("b.hstrace"))
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(second.status.code(), Some(1));
    assert!(
        stdout_text(&second).contains("already has"),
        "{}",
        stdout_text(&second)
    );
    growth.send("quit");
    assert_eq!(growth.child.wait().unwrap().code(), Some(0));
}

#[test]
fn a_killed_target_leaves_a_readable_trace_without_a_footer() {
    let Some(dir) = samples_dir() else { return };
    let work = scratch("killed");
    let trace = work.join("killed.hstrace");
    let mut run = Talker::spawn({
        let mut c = heapscope();
        c.arg("run")
            .arg("-o")
            .arg(&trace)
            .arg("--")
            .arg(sample(&dir, "growth"))
            .arg("--wait-for-attach");
        c
    });
    let pid = run
        .read_line()
        .strip_prefix("PID ")
        .expect("PID line")
        .to_string();
    run.send("grow 4");
    assert_eq!(run.read_line(), "grown 4");
    // Let the flush thread write a chunk (it does so every quarter second), then kill the target.
    std::thread::sleep(std::time::Duration::from_millis(900));
    let kill = std::process::Command::new("taskkill")
        .args(["/PID", &pid, "/F"])
        .output()
        .unwrap();
    assert!(kill.status.success(), "{}", stdout_text(&kill));
    let status = run.child.wait().unwrap();
    assert!(!status.success());

    let r = replay(&trace);
    assert!(
        r.footer.is_none(),
        "a killed process cannot have written a footer"
    );
    assert_eq!(r.count_allocs_of_size(65_536), 4);
}

#[test]
fn custom_pool_opt_in_shows_each_pool_object_with_its_call_path_and_markers() {
    let Some(dir) = samples_dir() else { return };
    let (r, expected_path, _) = run_sample(&dir, "custom-pool", "pool-optin", &[], &[]);
    assert_matches_expected(&r, &read_expected(&expected_path));
    let leak = r
        .pool_live
        .values()
        .find(|b| b.size == 64)
        .expect("pool_leak_site block")
        .stack;
    assert_eq!((r.live_at(leak).allocs, r.live_at(leak).bytes), (50, 3_200));
    assert!(r.stack_reaches_module(leak, "custom-pool.exe"));
    let labels: Vec<&str> = r.markers.iter().map(|(_, l)| l.as_str()).collect();
    assert_eq!(labels, ["pool-ready", "pool-done"]);
}

#[test]
fn header_compiles_as_c_and_macros_work_from_racing_threads() {
    let Some(dir) = samples_dir() else { return };
    // The same source, built as C11 and as C89/C90.
    for name in ["header-c", "header-c89"] {
        header_c_checks(&dir, name);
    }
}

fn header_c_checks(dir: &std::path::Path, name: &str) {
    if !sample(dir, name).is_file() {
        assert!(
            std::env::var_os("HEAPSCOPE_REQUIRE_SAMPLES").is_none(),
            "{name} sample is not built"
        );
        eprintln!("skipped: {name} sample is not built");
        return;
    }
    let (r, _, _) = run_sample(dir, name, name, &[], &[]);
    let threads = r.site_of_size(32, 400).expect("thread allocations");
    assert_eq!(
        (r.live_at(threads).allocs, r.live_at(threads).bytes),
        (400, 12_800)
    );
    let pool = r.site_of_size(48, 10).expect("pool_user_c allocations");
    assert_eq!((r.live_at(pool).allocs, r.live_at(pool).bytes), (5, 240));
    // The resize: old block freed, one 96-byte block live.
    let resized: Vec<_> = r.pool_live.values().filter(|b| b.size == 96).collect();
    assert_eq!(resized.len(), 1);
    assert!(r.stack_reaches_module(resized[0].stack, &format!("{name}.exe")));
    let live_of = |size| {
        let all = r.live.values().chain(r.pool_live.values());
        all.filter(|b| b.size == size).count()
    };
    // The CRT keeps a few live blocks of its own, so these are lower bounds (the exact counts per
    // call site are asserted above).
    assert!(live_of(32) >= 400 && live_of(48) >= 5);
    assert_eq!(live_of(96), 1);
    let labels: Vec<&str> = r.markers.iter().map(|(_, l)| l.as_str()).collect();
    assert_eq!(labels, ["c-start", "c-end"]);
}

// ---- H1: capture lifecycle (teardown, exceptions, memory after detach) ----

#[test]
fn dll_statics_freed_during_process_shutdown_leave_no_leak_candidate() {
    let Some(dir) = samples_dir() else { return };
    if !sample(&dir, "dll-statics").is_file() {
        assert!(
            std::env::var_os("HEAPSCOPE_REQUIRE_SAMPLES").is_none(),
            "dll-statics sample is not built"
        );
        return;
    }
    let work = scratch("dll-statics");
    let marker = work.join("destructor-ran.txt");
    let _ = std::fs::remove_file(&marker);
    let trace = work.join("dll-statics.hstrace");
    let out = heapscope()
        .env("HEAPSCOPE_DS_MARKER", &marker)
        .arg("run")
        .arg("-o")
        .arg(&trace)
        .arg("--")
        .arg(sample(&dir, "dll-statics"))
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stdout_text(&out));
    assert!(
        marker.is_file(),
        "the DLL's static destructor did not run, so this test proves nothing"
    );
    let r = replay(&trace);
    assert!(r.footer.is_some());
    let site = r.site_of_size(7_777, 1).expect("the DLL's allocation");
    assert!(r.stack_reaches_module(site, "dll_statics_lib.dll"));
    assert_eq!(
        r.live_at(site).allocs,
        0,
        "the block freed by the DLL's destructor is still live in the trace"
    );
}

#[test]
fn thread_churn_leaves_no_per_thread_live_blocks_and_no_missed_frees() {
    let Some(dir) = samples_dir() else { return };
    if !sample(&dir, "thread-churn").is_file() {
        assert!(
            std::env::var_os("HEAPSCOPE_REQUIRE_SAMPLES").is_none(),
            "thread-churn sample is not built"
        );
        return;
    }
    let (few, _, _) = run_sample(&dir, "thread-churn", "churn-few", &[], &["4"]);
    let (many, _, _) = run_sample(&dir, "thread-churn", "churn-many", &[], &["64"]);
    assert_eq!(many.footer.unwrap().dropped.overflow, 0);
    // Per-thread blocks (loader, FLS, CRT) must be freed in the trace as they are in the process, so
    // 60 more threads leave nothing more behind.
    assert!(
        many.live.len() <= few.live.len() + 2,
        "live blocks grow with thread count: {} (4 threads) vs {} (64 threads)",
        few.live.len(),
        many.live.len()
    );
    assert!(
        many.frees_without_alloc <= few.frees_without_alloc,
        "frees of unknown blocks grow with thread count: {} vs {}",
        few.frees_without_alloc,
        many.frees_without_alloc
    );
}

#[test]
fn a_heap_exception_caught_by_the_program_does_not_stop_recording() {
    let Some(dir) = samples_dir() else { return };
    if !sample(&dir, "heap-exception").is_file() {
        assert!(
            std::env::var_os("HEAPSCOPE_REQUIRE_SAMPLES").is_none(),
            "heap-exception sample is not built"
        );
        return;
    }
    let (r, _, out) = run_sample(&dir, "heap-exception", "heap-exception", &[], &[]);
    assert!(stdout_text(&out).contains("3 exceptions caught"));
    assert_eq!(
        r.count_allocs_of_size(3_333),
        50,
        "allocations after a caught heap exception were not recorded"
    );
    assert!(r.footer.is_some());
}

#[test]
fn attach_then_detach_gives_the_capture_buffers_back() {
    let Some(dir) = samples_dir() else { return };
    let work = scratch("attach-memory");
    let trace = work.join("growth-mem.hstrace");
    let mut growth = Talker::spawn({
        let mut c = std::process::Command::new(sample(&dir, "growth"));
        c.arg("--wait-for-attach");
        c
    });
    let pid: u32 = growth
        .read_line()
        .strip_prefix("PID ")
        .expect("PID line")
        .parse()
        .unwrap();
    let private = |g: &mut Talker| -> u64 {
        g.send("mem");
        g.read_line()
            .strip_prefix("private ")
            .expect("private line")
            .parse()
            .unwrap()
    };
    // Warm up the allocator so the baseline already holds what a grow/shrink cycle leaves behind.
    growth.send("grow 4");
    assert_eq!(growth.read_line(), "grown 4");
    growth.send("shrink 4");
    assert_eq!(growth.read_line(), "shrunk 4");
    let before = private(&mut growth);

    let mut att = Talker::spawn({
        let mut c = heapscope();
        c.args(["attach", "--pid", &pid.to_string(), "--until-eof", "-o"])
            .arg(&trace);
        c
    });
    att.read_until("attached to process");
    growth.send("grow 4");
    assert_eq!(growth.read_line(), "grown 4");
    growth.send("shrink 4");
    assert_eq!(growth.read_line(), "shrunk 4");
    att.close_stdin();
    att.read_until("detached from process");
    att.read_until("heapscope:");
    assert!(att.child.wait().unwrap().success());

    let after = private(&mut growth);
    growth.send("quit");
    assert_eq!(growth.child.wait().unwrap().code(), Some(0));
    let extra = after.saturating_sub(before);
    assert!(
        extra < 1_536 * 1024,
        "{extra} extra private bytes remain after detach (before {before}, after {after})"
    );
}

// ---- H3: capture accuracy (sampling, --min-lifetime, realloc ordering) ----

#[test]
fn sampling_estimates_a_hot_site_that_reuses_a_few_addresses() {
    let Some(dir) = samples_dir() else { return };
    // `churn_site` makes 50,000 allocations of 48 B and reuses the same 64 addresses every round.
    // Deciding by address kept all of those addresses or none; deciding per allocation does not.
    for one_in in [10u64, 16, 32, 64] {
        let (r, _, _) = run_sample(
            &dir,
            "churn",
            &format!("sample-hot-{one_in}"),
            &["--sample", &one_in.to_string()],
            &[],
        );
        assert_eq!(
            r.header.capture_mode,
            CaptureMode::Sampled {
                one_in: one_in as u32
            }
        );
        let site = r.site_of_size(48, 1).expect("churn_site stack");
        let estimate = r.allocated[&site].allocs * one_in;
        assert!(
            (42_500..=57_500).contains(&estimate),
            "--sample {one_in}: churn_site estimated at {estimate}, truth 50000"
        );
        assert_eq!(
            r.live_at(site).allocs,
            0,
            "a kept allocation must have its free kept too"
        );
        assert_eq!(
            r.footer.unwrap().dropped.overflow,
            0,
            "the table of kept addresses overflowed"
        );
    }
}

/// Live blocks of `r` that the ping-pong sample made itself: (64 B hand-offs, 96 B leaks, blocks of
/// 1 MiB or more).
fn ping_pong_live(r: &Replay) -> (usize, usize, usize) {
    let of = |size: u64| {
        r.live
            .values()
            .filter(|b| b.size == size && r.stack_reaches_module(b.stack, "ping-pong.exe"))
            .count()
    };
    let big = r.live.values().filter(|b| b.size >= 1 << 20).count();
    (of(64), of(96), big)
}

fn ping_pong_built(dir: &std::path::Path) -> bool {
    let built = sample(dir, "ping-pong").is_file();
    if !built {
        assert!(
            std::env::var_os("HEAPSCOPE_REQUIRE_SAMPLES").is_none(),
            "ping-pong sample is not built"
        );
    }
    built
}

#[test]
fn min_lifetime_keeps_the_leak_candidates_of_a_program_that_passes_blocks_between_threads() {
    let Some(dir) = samples_dir() else { return };
    if !ping_pong_built(&dir) {
        return;
    }
    let (full, expected_path, _) = run_sample(&dir, "ping-pong", "pp-full", &[], &[]);
    assert_matches_expected(&full, &read_expected(&expected_path));
    // Nothing the program handed over is live at exit; only the ten 96-byte leaks are.
    assert_eq!(ping_pong_live(&full), (0, 10, 0));

    let (cut, _, _) = run_sample(
        &dir,
        "ping-pong",
        "pp-cut",
        &["--min-lifetime", "50ms"],
        &[],
    );
    assert!(cut.footer.unwrap().dropped.min_lifetime > 100);
    assert_eq!(
        ping_pong_live(&cut),
        ping_pong_live(&full),
        "--min-lifetime changed which blocks look leaked"
    );
    assert!(
        cut.allocs_over_live <= full.allocs_over_live,
        "--min-lifetime lost frees: {} allocations over live blocks, {} in a full capture",
        cut.allocs_over_live,
        full.allocs_over_live
    );
}

#[test]
fn a_block_resized_while_other_threads_reuse_its_sizes_loses_no_free() {
    let Some(dir) = samples_dir() else { return };
    if !ping_pong_built(&dir) {
        return;
    }
    let (r, expected_path, _) = run_sample(&dir, "ping-pong", "realloc-stress", &[], &[]);
    // The resized block and the contenders end with nothing live, and no allocation landed on an
    // address the trace still held (which is what a free sorted behind a realloc looks like).
    assert_matches_expected(&r, &read_expected(&expected_path));
    assert_eq!(ping_pong_live(&r).2, 0, "large blocks are still live");
    assert!(
        r.allocs_over_live <= 2,
        "{} allocations on live addresses: frees were lost or reordered",
        r.allocs_over_live
    );
}

// ---- H4: C API header and DLL selection ----

#[test]
fn a_program_marker_before_the_attach_does_not_silence_the_header_after_it() {
    let Some(dir) = samples_dir() else { return };
    let work = scratch("header-attach");
    let trace = work.join("growth-header.hstrace");
    let mut growth = Talker::spawn({
        let mut c = std::process::Command::new(sample(&dir, "growth"));
        c.arg("--wait-for-attach");
        c
    });
    let pid: u32 = growth
        .read_line()
        .strip_prefix("PID ")
        .expect("PID line")
        .parse()
        .unwrap();
    // Before the attach there is no DLL: the macro finds nothing and must look again later.
    growth.send("mark before-attach");
    assert_eq!(growth.read_line(), "marked");

    let mut att = Talker::spawn({
        let mut c = heapscope();
        c.args(["attach", "--pid", &pid.to_string(), "--until-eof", "-o"])
            .arg(&trace);
        c
    });
    att.read_until("attached to process");
    // The header looks again about every 250 ms.
    std::thread::sleep(std::time::Duration::from_millis(600));
    growth.send("mark after-attach");
    assert_eq!(growth.read_line(), "marked");
    att.close_stdin();
    att.read_until("detached from process");
    att.read_until("heapscope:");
    assert!(att.child.wait().unwrap().success());
    growth.send("quit");
    assert_eq!(growth.child.wait().unwrap().code(), Some(0));

    let r = replay(&trace);
    let labels: Vec<&str> = r.markers.iter().map(|(_, l)| l.as_str()).collect();
    assert_eq!(labels, ["after-attach"], "markers in the trace");
}

#[test]
fn the_header_does_not_break_cxx_code_around_it_in_either_include_order() {
    let Some(dir) = samples_dir() else { return };
    if !sample(&dir, "header-cxx").is_file() {
        assert!(
            std::env::var_os("HEAPSCOPE_REQUIRE_SAMPLES").is_none(),
            "header-cxx sample is not built"
        );
        return;
    }
    // Compiling it is most of the test (min/max macros, WIN32_LEAN_AND_MEAN, shellapi.h); running it
    // checks that the macros evaluate their arguments at most once, with and without HeapScope.
    let plain = std::process::Command::new(sample(&dir, "header-cxx"))
        .output()
        .unwrap();
    assert!(plain.status.success(), "{}", stdout_text(&plain));
    let (r, _, out) = run_sample(&dir, "header-cxx", "header-cxx", &[], &[]);
    assert!(out.status.success());
    let labels: Vec<&str> = r.markers.iter().map(|(_, l)| l.as_str()).collect();
    assert_eq!(labels, ["cxx-main", "cxx-other"]);
}

#[test]
fn the_dll_override_must_be_a_full_path_to_an_existing_file() {
    let run = |dll: &str| {
        heapscope()
            .env("HEAPSCOPE_DLL", dll)
            .args(["run", "--", "cmd.exe", "/c", "exit", "0"])
            .output()
            .unwrap()
    };
    for relative in [
        "heapscope_capture.dll",
        ".\\heapscope_capture.dll",
        "\\x.dll",
    ] {
        let out = run(relative);
        assert_eq!(out.status.code(), Some(1), "{relative}");
        assert!(
            stdout_text(&out).contains("must be a full path"),
            "{relative}: {}",
            stdout_text(&out)
        );
    }
    let out = run("C:\\no\\such\\dir\\heapscope_capture.dll");
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stdout_text(&out).contains("does not exist"),
        "{}",
        stdout_text(&out)
    );

    // The real DLL by full path works.
    let dll = std::path::Path::new(env!("CARGO_BIN_EXE_heapscope"))
        .with_file_name("heapscope_capture.dll");
    if dll.is_file() {
        let out = run(dll.to_str().unwrap());
        assert!(out.status.success(), "{}", stdout_text(&out));
    }
}

#[test]
fn a_dll_of_another_control_version_says_so_instead_of_blaming_antivirus() {
    let out = heapscope()
        .env("HEAPSCOPE_TEST_CONTROL_VERSION", "77")
        .args(["run", "--", "cmd.exe", "/c", "exit", "0"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let text = stdout_text(&out);
    assert!(
        text.contains("different HeapScope build")
            && text.contains("version 2")
            && text.contains("version 77"),
        "{text}"
    );
    assert!(!text.contains("antivirus"), "{text}");
}

// ---- H5: launcher robustness ----

fn start_growth() -> (Talker, u32) {
    let dir = samples_dir().expect("samples");
    let mut growth = Talker::spawn({
        let mut c = std::process::Command::new(sample(&dir, "growth"));
        c.arg("--wait-for-attach");
        c
    });
    let pid = growth
        .read_line()
        .strip_prefix("PID ")
        .expect("PID line")
        .parse()
        .unwrap();
    (growth, pid)
}

fn wait_exit(child: &mut std::process::Child, secs: u64) -> std::process::ExitStatus {
    let end = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    loop {
        if let Some(s) = child.try_wait().unwrap() {
            return s;
        }
        assert!(
            std::time::Instant::now() < end,
            "process did not exit in {secs} s"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// Poll until the trace has a footer (the DLL closed it), and return its size then.
fn wait_for_footer(trace: &std::path::Path, secs: u64) -> u64 {
    let end = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    loop {
        let closed = std::panic::catch_unwind(|| replay(trace).footer.is_some()).unwrap_or(false);
        if closed {
            return std::fs::metadata(trace).unwrap().len();
        }
        assert!(
            std::time::Instant::now() < end,
            "the trace was never closed: the DLL is still capturing"
        );
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
}

#[test]
fn run_refuses_batch_files_and_does_not_search_the_current_directory() {
    let work = scratch("launcher-resolve");
    let bat = work.join("x.bat");
    std::fs::write(&bat, "@echo hi\r\n").unwrap();
    let out = heapscope().args(["run", "--"]).arg(&bat).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(stdout_text(&out).contains("batch"), "{}", stdout_text(&out));

    // A bare name that exists only in the current directory is not run.
    std::fs::copy(env!("CARGO_BIN_EXE_heapscope"), work.join("only-here.exe")).unwrap();
    let out = heapscope()
        .current_dir(&work)
        .args(["run", "--", "only-here"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stdout_text(&out).contains("not found on PATH"),
        "{}",
        stdout_text(&out)
    );
}

#[test]
fn two_runs_into_one_trace_file_fail_cleanly_and_the_first_is_unharmed() {
    let Some(dir) = samples_dir() else { return };
    let work = scratch("launcher-same-output");
    let trace = work.join("same.hstrace");
    let mut first = Talker::spawn({
        let mut c = heapscope();
        c.arg("run")
            .arg("-o")
            .arg(&trace)
            .arg("--")
            .arg(sample(&dir, "growth"))
            .arg("--wait-for-attach");
        c
    });
    first.read_line(); // PID: the first capture is up and holds the file
    let second = heapscope()
        .arg("run")
        .arg("-o")
        .arg(&trace)
        .arg("--")
        .arg(sample(&dir, "leak"))
        .output()
        .unwrap();
    assert_eq!(second.status.code(), Some(1));
    assert!(
        stdout_text(&second).contains("another capture is writing"),
        "{}",
        stdout_text(&second)
    );
    first.send("grow 2");
    assert_eq!(first.read_line(), "grown 2");
    first.send("quit");
    assert_eq!(first.child.wait().unwrap().code(), Some(0));
    let r = replay(&trace);
    assert!(r.footer.is_some());
    assert_eq!(r.count_allocs_of_size(65_536), 2);
}

#[test]
fn attach_says_exited_when_the_target_ends() {
    let Some(_) = samples_dir() else { return };
    let work = scratch("attach-exited");
    let trace = work.join("exited.hstrace");
    let (mut growth, pid) = start_growth();
    let mut att = Talker::spawn({
        let mut c = heapscope();
        c.args(["attach", "--pid", &pid.to_string(), "-o"])
            .arg(&trace);
        c
    });
    att.read_until("attached to process");
    growth.send("grow 2");
    assert_eq!(growth.read_line(), "grown 2");
    growth.send("quit");
    assert_eq!(growth.child.wait().unwrap().code(), Some(0));
    let line = att.read_until("while capturing");
    assert!(line.contains("exited"), "{line}");
    assert!(wait_exit(&mut att.child, 20).success());
}

#[test]
fn attach_with_a_closed_stdout_ends_cleanly_and_leaves_the_target_dormant() {
    let Some(_) = samples_dir() else { return };
    let work = scratch("attach-closed-stdout");
    let trace = work.join("closed.hstrace");
    let (mut growth, pid) = start_growth();
    let Talker {
        mut child,
        stdin,
        mut stdout,
    } = Talker::spawn({
        let mut c = heapscope();
        c.args([
            "attach",
            "--pid",
            &pid.to_string(),
            "--duration",
            "3s",
            "-o",
        ])
        .arg(&trace);
        c
    });
    let mut first = String::new();
    stdout.read_line(&mut first).unwrap();
    assert!(first.contains("attached to process"), "{first}");
    drop(stdout); // nobody reads the output any more
    let status = wait_exit(&mut child, 20);
    assert!(status.success(), "the CLI did not end cleanly: {status:?}");
    drop(stdin);

    let size = wait_for_footer(&trace, 10);
    growth.send("grow 4");
    assert_eq!(growth.read_line(), "grown 4");
    std::thread::sleep(std::time::Duration::from_millis(600));
    assert_eq!(
        std::fs::metadata(&trace).unwrap().len(),
        size,
        "the trace kept growing"
    );
    growth.send("quit");
    assert_eq!(growth.child.wait().unwrap().code(), Some(0));
}

#[test]
fn a_killed_attach_cli_leaves_the_target_dormant_within_seconds() {
    let Some(_) = samples_dir() else { return };
    let work = scratch("attach-cli-killed");
    let trace = work.join("killed-cli.hstrace");
    let (mut growth, pid) = start_growth();
    let mut att = Talker::spawn({
        let mut c = heapscope();
        c.args(["attach", "--pid", &pid.to_string(), "-o"])
            .arg(&trace);
        c
    });
    att.read_until("attached to process");
    growth.send("grow 3");
    assert_eq!(growth.read_line(), "grown 3");
    att.child.kill().unwrap();
    att.child.wait().unwrap();

    // The DLL notices the controller is gone, finishes the trace and stops recording.
    let size = wait_for_footer(&trace, 15);
    growth.send("grow 4");
    assert_eq!(growth.read_line(), "grown 4");
    std::thread::sleep(std::time::Duration::from_millis(600));
    assert_eq!(
        std::fs::metadata(&trace).unwrap().len(),
        size,
        "the trace kept growing"
    );
    assert_eq!(replay(&trace).count_allocs_of_size(65_536), 3);
    growth.send("quit");
    assert_eq!(growth.child.wait().unwrap().code(), Some(0));
}
