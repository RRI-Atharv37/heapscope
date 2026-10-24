//! `heapscope diff before.hstrace after.hstrace`, or `heapscope diff trace.hstrace --markers A B`
//!
//! Lists the allocation sites whose live memory grew, shrank, appeared or disappeared. Sites are
//! matched by symbol, so the two traces can come from different runs or builds.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use heapscope_analysis::diff::{self, Change, Diff, DiffOptions, Point};
use heapscope_analysis::{analyze, Analysis, Options, Trace};
use heapscope_symbols::{Options as SymOptions, Symbolizer};

use super::{bytes, signed_bytes};

pub struct DiffArgs {
    pub before: PathBuf,
    /// `None` when comparing two markers of `before`.
    pub after: Option<PathBuf>,
    pub markers: Option<(String, String)>,
    pub limit: usize,
    pub min_change: u64,
    pub depth: usize,
    pub include_unchanged: bool,
}

fn load(path: &Path) -> Result<Analysis, String> {
    let trace =
        Trace::open(path).map_err(|e| format!("cannot read trace {}: {e}", path.display()))?;
    let mut symbols = Symbolizer::new(trace.modules.clone(), SymOptions::standard());
    let analysis = analyze(&trace, &mut symbols, &Options::default());
    for w in &analysis.warnings {
        eprintln!("heapscope: {}: {w}", path.display());
    }
    Ok(analysis)
}

/// A marker label, or `@end` for the end of the trace.
fn point(a: &Analysis, label: &str) -> Result<Point, String> {
    if label == "@end" {
        return Ok(Point::End);
    }
    diff::find_marker(a, label)
        .map(Point::Marker)
        .ok_or_else(|| {
            let known: Vec<String> = a.markers.iter().map(|m| format!("'{}'", m.label)).collect();
            if known.is_empty() {
                format!("the trace has no marker '{label}' (it has no markers at all)")
            } else {
                format!(
                "the trace has no marker '{label}' (markers: {}; '@end' is the end of the trace)",
                known.join(", ")
            )
            }
        })
}

pub fn run(args: DiffArgs) -> Result<ExitCode, String> {
    let opts = DiffOptions {
        depth: args.depth,
        min_change_bytes: args.min_change,
        include_unchanged: args.include_unchanged,
        ..DiffOptions::default()
    };
    let (d, title) = match (&args.after, &args.markers) {
        (Some(after), None) => {
            let a = load(&args.before)?;
            let b = load(after)?;
            let title = format!("{} -> {}", args.before.display(), after.display());
            (diff::diff(&a, &b, &opts), title)
        }
        (None, Some((from, to))) => {
            let a = load(&args.before)?;
            let (p, q) = (point(&a, from)?, point(&a, to)?);
            let d = diff::diff_points(&a, p, q, &opts)?;
            (
                d,
                format!("{}: marker '{from}' -> '{to}'", args.before.display()),
            )
        }
        (Some(_), Some(_)) => {
            return Err("--markers compares two points of one trace; give one trace file".into())
        }
        (None, None) => {
            return Err(
                "give a second trace file, or --markers FROM TO to compare two markers of \
                 one trace"
                    .into(),
            )
        }
    };
    print(&d, &title, args.limit);
    Ok(ExitCode::SUCCESS)
}

fn print(d: &Diff, title: &str, limit: usize) {
    let est = if d.estimated { " (estimated)" } else { "" };
    println!("Live memory diff{est}: {title}");
    println!(
        "  before: {} in {} allocations",
        bytes(d.before.bytes),
        d.before.allocs
    );
    println!(
        "  after:  {} in {} allocations ({})",
        bytes(d.after.bytes),
        d.after.allocs,
        signed_bytes(d.delta_bytes())
    );
    for n in &d.notes {
        println!("  note: {n}");
    }
    if d.entries.is_empty() {
        println!("\nNo allocation site changed.");
        return;
    }
    println!(
        "\n  {} grew, {} new, {} shrank, {} gone",
        d.count(Change::Grew),
        d.count(Change::New),
        d.count(Change::Shrank),
        d.count(Change::Gone)
    );
    let shown = if limit == 0 {
        d.entries.len()
    } else {
        limit.min(d.entries.len())
    };
    for e in &d.entries[..shown] {
        println!(
            "\n{:<7} {:>11}  {} -> {}  ({:+} allocations)",
            e.change.label(),
            signed_bytes(e.delta_bytes()),
            bytes(e.before.bytes),
            bytes(e.after.bytes),
            e.delta_allocs()
        );
        println!("        {}", e.path);
        if let Some(loc) = &e.location {
            println!("        at {loc}");
        }
    }
    if shown < d.entries.len() {
        println!(
            "\n... and {} more (use --limit 0 to show all)",
            d.entries.len() - shown
        );
    }
}
