//! Ground-truth end-to-end tests (S2.5): run each sample under `heapscope run`, summarise the
//! trace with `heapscope analyze -o`, and compare the JSON with the `expected.json` the sample
//! wrote about its own deliberate allocations.

#[allow(dead_code)]
mod support;

use std::path::Path;

use heapscope_analysis::json::Json;
use support::*;

struct Truth {
    function: String,
    allocs: u64,
    alloc_bytes: u64,
    frees: u64,
    freed_bytes: u64,
    live_allocs: u64,
    live_bytes: u64,
}

fn num(v: &Json, key: &str) -> u64 {
    v.get(key)
        .and_then(Json::as_u64)
        .unwrap_or_else(|| panic!("no number '{key}'"))
}

fn read_truth(path: &Path) -> Vec<Truth> {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let json = Json::parse(&text).expect("expected.json is valid JSON");
    json.get("sites")
        .and_then(Json::as_array)
        .expect("expected.json has sites")
        .iter()
        .map(|s| Truth {
            function: s
                .get("function")
                .and_then(Json::as_str)
                .unwrap()
                .to_string(),
            allocs: num(s, "allocs"),
            alloc_bytes: num(s, "alloc_bytes"),
            frees: num(s, "frees"),
            freed_bytes: num(s, "freed_bytes"),
            live_allocs: num(s, "live_allocs_at_exit"),
            live_bytes: num(s, "live_bytes_at_exit"),
        })
        .collect()
}

/// Run `name` under heapscope, analyse it, and return `(summary, truth)`.
fn summarise(dir: &Path, name: &str) -> (Json, Vec<Truth>) {
    let work = scratch(&format!("e2e-{name}"));
    let trace = work.join("t.hstrace");
    let expected = work.join("expected.json");
    let out = heapscope()
        .arg("run")
        .arg("-o")
        .arg(&trace)
        .arg("--")
        .arg(sample(dir, name))
        .arg("--expected")
        .arg(&expected)
        .output()
        .unwrap_or_else(|e| panic!("cannot start heapscope ({e})"));
    assert!(out.status.success(), "run failed: {}", stdout_text(&out));

    let summary = work.join("summary.json");
    let out = heapscope()
        .arg("analyze")
        .arg(&trace)
        .arg("-o")
        .arg(&summary)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "analyze failed: {}",
        stdout_text(&out)
    );
    let json = Json::parse(&std::fs::read_to_string(&summary).unwrap()).expect("summary parses");
    (json, read_truth(&expected))
}

/// Sum of the sites of the summary whose function is `function`.
fn site_totals(summary: &Json, function: &str) -> [u64; 6] {
    let mut sum = [0u64; 6];
    let mut found = false;
    for s in summary.get("sites").and_then(Json::as_array).unwrap() {
        if s.get("function").and_then(Json::as_str) != Some(function) {
            continue;
        }
        found = true;
        for (slot, key) in sum.iter_mut().zip([
            "allocs",
            "alloc_bytes",
            "frees",
            "freed_bytes",
            "live_allocs",
            "live_bytes",
        ]) {
            *slot += num(s, key);
        }
    }
    assert!(found, "no site '{function}' in the summary");
    sum
}

fn check_sites(name: &str, summary: &Json, truth: &[Truth]) {
    assert_eq!(num(summary, "schema_version"), 1);
    let cap = summary.get("capture").unwrap();
    assert_eq!(cap.get("mode").and_then(Json::as_str), Some("full"));
    assert_eq!(cap.get("estimated"), Some(&Json::Bool(false)), "{name}");
    assert_eq!(cap.get("complete"), Some(&Json::Bool(true)), "{name}");
    assert_eq!(num(cap.get("dropped").unwrap(), "overflow"), 0, "{name}");

    for t in truth {
        let got = site_totals(summary, &t.function);
        assert_eq!(
            got,
            [
                t.allocs,
                t.alloc_bytes,
                t.frees,
                t.freed_bytes,
                t.live_allocs,
                t.live_bytes
            ],
            "{name}: site {} (allocs, alloc_bytes, frees, freed_bytes, live_allocs, live_bytes)",
            t.function
        );
    }
}

/// Every sample site that still holds memory at exit is a leak candidate with the same totals, and
/// no candidate is a site the sample freed.
fn check_leaks(name: &str, summary: &Json, truth: &[Truth]) {
    let leaks = summary.get("leaks").unwrap();
    assert_eq!(leaks.get("kind").and_then(Json::as_str), Some("leaks"));
    let entries = leaks.get("entries").and_then(Json::as_array).unwrap();
    for t in truth {
        let mine: Vec<&Json> = entries
            .iter()
            .filter(|e| e.get("function").and_then(Json::as_str) == Some(t.function.as_str()))
            .collect();
        let (allocs, bytes) = mine.iter().fold((0, 0), |(a, b), e| {
            (a + num(e, "allocs"), b + num(e, "bytes"))
        });
        assert_eq!(
            (allocs, bytes),
            (t.live_allocs, t.live_bytes),
            "{name}: leak candidates for {}",
            t.function
        );
    }
    // Sorted largest first.
    let sizes: Vec<u64> = entries.iter().map(|e| num(e, "bytes")).collect();
    assert!(sizes.windows(2).all(|w| w[0] >= w[1]), "{name}: {sizes:?}");
}

fn check_peak(name: &str, summary: &Json) {
    let peak = num(summary.get("peak").unwrap(), "live_bytes");
    let live = num(summary.get("totals").unwrap(), "live_bytes");
    let total = num(summary.get("totals").unwrap(), "alloc_bytes");
    assert!(peak >= live && peak <= total, "{name}: peak {peak}");
}

fn run_ground_truth(name: &str, leaks_expected: bool) {
    let Some(dir) = samples_dir() else { return };
    let (summary, truth) = summarise(&dir, name);
    assert!(!truth.is_empty(), "{name}: expected.json lists no sites");
    check_sites(name, &summary, &truth);
    check_leaks(name, &summary, &truth);
    check_peak(name, &summary);
    let any_live = truth.iter().any(|t| t.live_allocs > 0);
    assert_eq!(any_live, leaks_expected, "{name}");
}

#[test]
fn leak_sample_matches_its_ground_truth() {
    run_ground_truth("leak", true);
    let Some(dir) = samples_dir() else { return };
    let (summary, _) = summarise(&dir, "leak");
    // The balanced site is not a leak candidate at all.
    let entries = summary
        .get("leaks")
        .and_then(|l| l.get("entries"))
        .and_then(Json::as_array)
        .unwrap();
    assert!(!entries
        .iter()
        .any(|e| e.get("function").and_then(Json::as_str) == Some("balanced_site")));
    // The two leaks come first and the first is the 640 KiB one.
    assert_eq!(
        entries[0].get("function").and_then(Json::as_str),
        Some("leak_site_b")
    );
    assert_eq!(num(&entries[0], "bytes"), 655_360);
    let file = entries[0].get("file").and_then(Json::as_str).unwrap();
    assert!(file.replace('\\', "/").ends_with("samples/leak/main.cpp"));
}

#[test]
fn basic_alloc_sample_matches_its_ground_truth() {
    run_ground_truth("basic-alloc", false);
}

#[test]
fn multithread_sample_matches_its_ground_truth() {
    run_ground_truth("multithread", true);
}

#[test]
fn churn_sample_matches_its_ground_truth_and_reports_the_churn_site() {
    run_ground_truth("churn", true);
    let Some(dir) = samples_dir() else { return };
    let (summary, _) = summarise(&dir, "churn");
    let churn = summary.get("churn").and_then(Json::as_array).unwrap();
    assert_eq!(
        churn[0].get("function").and_then(Json::as_str),
        Some("churn_site")
    );
}

#[test]
fn custom_pool_sample_matches_its_ground_truth() {
    run_ground_truth("custom-pool", true);
    let Some(dir) = samples_dir() else { return };
    let (summary, _) = summarise(&dir, "custom-pool");
    // The pool objects live inside the 64 KiB chunk, slot 0 at the chunk's own address: neither
    // may replace the other in the live set, so the chunk is still there to be freed at the end.
    let chunk: Vec<&Json> = summary
        .get("sites")
        .and_then(Json::as_array)
        .unwrap()
        .iter()
        .filter(|s| num(s, "alloc_bytes") == 65_536)
        .collect();
    assert_eq!(chunk.len(), 1, "the pool's backing block");
    assert_eq!((num(chunk[0], "allocs"), num(chunk[0], "frees")), (1, 1));
    let peak = num(summary.get("peak").unwrap(), "live_bytes");
    assert!(
        peak >= 65_536 + 50 * 64,
        "chunk and pool objects together: {peak}"
    );
}

#[test]
fn private_heap_sample_has_no_leak_after_heap_destroy() {
    run_ground_truth("private-heap", false);
    let Some(dir) = samples_dir() else { return };
    let (summary, _) = summarise(&dir, "private-heap");
    let leaks = summary.get("leaks").unwrap();
    let entries = leaks.get("entries").and_then(Json::as_array).unwrap();
    assert!(
        !entries
            .iter()
            .any(|e| e.get("function").and_then(Json::as_str) == Some("arena_site")),
        "blocks of a destroyed heap are still reported live"
    );
}

#[test]
fn growth_sample_matches_its_ground_truth_and_peaks_after_phase_two() {
    run_ground_truth("growth", true);
    let Some(dir) = samples_dir() else { return };
    let (summary, _) = summarise(&dir, "growth");
    // Phase 1 (4 MiB) and phase 2 (8 MiB) are live together before phase 3 frees phase 1.
    let peak = num(summary.get("peak").unwrap(), "live_bytes");
    let twelve_mib = 12 * 1024 * 1024;
    assert!(
        (twelve_mib..twelve_mib + 512 * 1024).contains(&peak),
        "peak {peak} is not about 12 MiB"
    );
    let live = num(summary.get("totals").unwrap(), "live_bytes");
    assert!(
        live < peak && live >= 8 * 1024 * 1024,
        "live at exit {live}"
    );
}

/// `(label, site line)` of each entry of the text output of `heapscope diff`.
fn diff_entries(text: &str) -> Vec<(String, String)> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        let label = l.split_whitespace().next().unwrap_or("");
        if ["new", "grew", "shrank", "gone"].contains(&label) && i + 1 < lines.len() {
            out.push((label.to_string(), lines[i + 1].trim().to_string()));
        }
    }
    out
}

fn record(dir: &Path, name: &str, tag: &str) -> std::path::PathBuf {
    let work = scratch(&format!("e2e-diff-{tag}"));
    let trace = work.join(format!("{name}.hstrace"));
    let out = heapscope()
        .arg("run")
        .arg("-o")
        .arg(&trace)
        .arg("--")
        .arg(sample(dir, name))
        .arg("--expected")
        .arg(work.join("expected.json"))
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stdout_text(&out));
    trace
}

#[test]
fn diff_of_two_real_runs_ignores_addresses_and_finds_the_changed_sites() {
    let Some(dir) = samples_dir() else { return };
    let (leak1, leak2) = (record(&dir, "leak", "a"), record(&dir, "leak", "b"));
    let growth = record(&dir, "growth", "c");
    let diff = |a: &Path, b: &Path| {
        let out = heapscope()
            .arg("diff")
            .arg(a)
            .arg(b)
            .args(["--limit", "0"])
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", stdout_text(&out));
        diff_entries(&String::from_utf8_lossy(&out.stdout))
    };

    // Two runs of the same program differ in every address, but not in any leak site.
    let same = diff(&leak1, &leak2);
    assert!(
        !same.iter().any(|(_, site)| site.starts_with("leak_site_")),
        "unchanged sites reported as changed: {same:?}"
    );

    // leak -> growth: the leaks are gone, the growth phases are new.
    let changed = diff(&leak1, &growth);
    let has = |label: &str, site: &str| {
        changed
            .iter()
            .any(|(l, s)| l == label && s.starts_with(site))
    };
    assert!(has("gone", "leak_site_a"), "{changed:?}");
    assert!(has("gone", "leak_site_b"), "{changed:?}");
    assert!(has("new", "grow_phase_2"), "{changed:?}");
    assert!(
        !changed.iter().any(|(_, s)| s.starts_with("balanced_site")),
        "a site with nothing live was reported: {changed:?}"
    );
}
