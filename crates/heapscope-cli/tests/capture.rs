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
        .live
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
    if !sample(&dir, "header-c").is_file() {
        assert!(
            std::env::var_os("HEAPSCOPE_REQUIRE_SAMPLES").is_none(),
            "header-c sample is not built"
        );
        eprintln!("skipped: header-c sample is not built");
        return;
    }
    let (r, _, _) = run_sample(&dir, "header-c", "header-c", &[], &[]);
    let threads = r.site_of_size(32, 400).expect("thread allocations");
    assert_eq!(
        (r.live_at(threads).allocs, r.live_at(threads).bytes),
        (400, 12_800)
    );
    let pool = r.site_of_size(48, 10).expect("pool_user_c allocations");
    assert_eq!((r.live_at(pool).allocs, r.live_at(pool).bytes), (5, 240));
    // The resize: old block freed, one 96-byte block live.
    let resized: Vec<_> = r.live.values().filter(|b| b.size == 96).collect();
    assert_eq!(resized.len(), 1);
    assert!(r.stack_reaches_module(resized[0].stack, "header-c.exe"));
    let live_of = |size| r.live.values().filter(|b| b.size == size).count();
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
