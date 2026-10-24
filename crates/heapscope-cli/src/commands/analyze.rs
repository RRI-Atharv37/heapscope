//! `heapscope analyze trace.hstrace [-o summary.json]`
//!
//! Prints the headline numbers, the top allocation sites and the leak candidates of a trace, and
//! optionally writes the versioned JSON summary (`docs/json-schema.md`) that tools such as the CI
//! gate read.

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use heapscope_analysis::json::{self, site_location};
use heapscope_analysis::leaks::{self, LeakEntry, LeakReport};
use heapscope_analysis::{analyze, Analysis, NodeId, Options, Suppressions, Trace};
use heapscope_symbols::{Options as SymOptions, Symbolizer};

use super::bytes;

pub struct AnalyzeArgs {
    pub trace: PathBuf,
    /// JSON summary destination; `-` writes it to standard output instead of the console report.
    pub output: Option<PathBuf>,
    pub suppressions: Option<PathBuf>,
    pub no_builtin_suppressions: bool,
    pub limit: usize,
}

pub fn run(args: AnalyzeArgs) -> Result<ExitCode, String> {
    let trace = Trace::open(&args.trace)
        .map_err(|e| format!("cannot read trace {}: {e}", args.trace.display()))?;
    let mut symbols = Symbolizer::new(trace.modules.clone(), SymOptions::standard());
    let analysis = analyze(&trace, &mut symbols, &Options::default());

    let mut rules = if args.no_builtin_suppressions {
        Suppressions::none()
    } else {
        Suppressions::builtin()
    };
    if let Some(path) = &args.suppressions {
        rules.extend(Suppressions::load(path)?);
    }
    let report = leaks::detect(&analysis, &rules);

    let to_stdout = args.output.as_deref().is_some_and(|p| p.as_os_str() == "-");
    if let Some(path) = &args.output {
        let text = json::summary(&analysis, &report).to_pretty();
        if to_stdout {
            std::io::stdout()
                .write_all(text.as_bytes())
                .map_err(|e| format!("cannot write to standard output: {e}"))?;
        } else {
            std::fs::write(path, text)
                .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        }
    }
    // The console report would corrupt JSON on stdout, so warnings go to stderr in that case.
    if to_stdout {
        for w in &analysis.warnings {
            eprintln!("heapscope: warning: {w}");
        }
    } else {
        print(&analysis, &report, &args);
    }
    Ok(ExitCode::SUCCESS)
}

fn n(analysis: &Analysis, v: u64) -> u64 {
    v * analysis.scale()
}

fn print(a: &Analysis, report: &LeakReport, args: &AnalyzeArgs) {
    let c = &a.capture;
    let est = if c.is_estimate() { " (estimated)" } else { "" };
    println!("HeapScope analysis of {}", args.trace.display());
    println!("  program: {}", c.command_line);
    print!("  capture: {}", c.mode());
    if c.sampled {
        print!(", 1 in {} allocations", c.sample_rate);
    }
    if c.min_lifetime_ms > 0 {
        print!(", short-lived under {} ms folded", c.min_lifetime_ms);
    }
    println!(
        "{}",
        if c.complete {
            ""
        } else {
            ", NOT shut down cleanly"
        }
    );

    let t = &a.totals;
    println!("\nTotals{est}");
    println!(
        "  allocated: {} in {} allocations",
        bytes(n(a, t.alloc_bytes)),
        n(a, t.allocs)
    );
    println!(
        "  freed:     {} in {} frees",
        bytes(n(a, t.freed_bytes)),
        n(a, t.frees)
    );
    println!(
        "  live at end: {} in {} allocations",
        bytes(n(a, t.live_bytes)),
        n(a, t.live_allocs)
    );
    println!(
        "  peak live: {} in {} allocations, at {:.3} s",
        bytes(n(a, a.peak.live_bytes)),
        n(a, a.peak.live_allocs),
        a.peak.ts_ns as f64 / 1e9
    );

    let top = a.top_sites(|s| s.alloc_bytes, limit_of(args.limit));
    if !top.is_empty() {
        println!("\nTop allocation sites by bytes allocated{est}");
        println!(
            "  {:>12} {:>10} {:>12}  site",
            "allocated", "count", "live now"
        );
        for id in top {
            let s = &a.stats[id as usize];
            println!(
                "  {:>12} {:>10} {:>12}  {}",
                bytes(n(a, s.alloc_bytes)),
                n(a, s.allocs),
                bytes(n(a, s.live_bytes)),
                site_line(a, id)
            );
        }
    }

    print_leaks(a, report, args.limit);

    if !a.churn.is_empty() {
        println!(
            "\nShort-lived allocations (freed within {} ms){est}",
            a.short_lived_ns as f64 / 1e6
        );
        for e in a.churn.iter().take(limit_of(args.limit).min(5)) {
            println!(
                "  {:>10} allocations ({:.0}% of the site), {} total, avg {:.1} us  {}",
                n(a, e.allocs),
                e.share * 100.0,
                bytes(n(a, e.bytes)),
                e.avg_lifetime_ns as f64 / 1e3,
                site_line(a, e.node)
            );
        }
    }

    if !a.markers.is_empty() {
        println!("\nMarkers");
        for m in &a.markers {
            println!(
                "  {:>9.3} s  {:<24} live {}",
                m.ts_ns as f64 / 1e9,
                m.label,
                bytes(n(a, m.live_bytes))
            );
        }
    }

    if !a.warnings.is_empty() {
        println!();
        for w in &a.warnings {
            println!("warning: {w}");
        }
    }
}

fn limit_of(limit: usize) -> usize {
    if limit == 0 {
        usize::MAX
    } else {
        limit
    }
}

/// `function  (file:line)  <- caller <- caller`
fn site_line(a: &Analysis, node: NodeId) -> String {
    let frame = &a.tree.node(node).frame;
    let mut out = frame.name.clone();
    if let Some(loc) = site_location(a, node) {
        out.push_str(&format!("  ({loc})"));
    }
    let callers = a.tree.path_string(node, 4);
    if let Some((_, rest)) = callers.split_once(" <- ") {
        out.push_str(&format!("  <- {rest}"));
    }
    out
}

fn print_leaks(a: &Analysis, r: &LeakReport, limit: usize) {
    println!("\n{}", r.title());
    for note in &r.notes {
        println!("  note: {note}");
    }
    if r.entries.is_empty() {
        println!("  none");
    } else {
        println!(
            "  {} in {} allocations over {} call paths",
            bytes(r.total_bytes),
            r.total_allocs,
            r.entries.len()
        );
        for k in &r.by_kind {
            println!(
                "    {:<8} {:>12} in {} allocations",
                k.kind.label(),
                bytes(k.bytes),
                k.allocs
            );
        }
        for e in r.entries.iter().take(limit_of(limit)) {
            print_entry(a, e);
        }
        if r.entries.len() > limit_of(limit) {
            println!(
                "\n  ... and {} more (use --limit 0 to show all)",
                r.entries.len() - limit
            );
        }
    }
    if !r.suppressed.is_empty() {
        println!(
            "\nSuppressed ({} in {} allocations)",
            bytes(r.suppressed_bytes),
            r.suppressed_allocs
        );
        for e in r.suppressed.iter().take(limit_of(limit).min(5)) {
            print_entry(a, e);
        }
        if r.suppressed.len() > 5 {
            println!("  ... {} more suppressed", r.suppressed.len() - 5);
        }
    }
}

fn print_entry(a: &Analysis, e: &LeakEntry) {
    let size = if e.kind == heapscope_analysis::ObjectKind::Handle {
        format!("{} handles", e.allocs)
    } else {
        format!("{} in {} allocations", bytes(e.bytes), e.allocs)
    };
    println!("\n  {:<7} {}", e.kind.label(), size);
    println!("          {}", site_line(a, e.node));
    if let Some(by) = &e.suppressed_by {
        println!("          suppressed by {}: {}", by.rule, by.reason);
    }
}
