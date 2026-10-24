//! `heapscope report trace.hstrace -o report.html`
//!
//! Writes one self-contained HTML page (summary, call tree, flame graph) that opens offline in
//! any browser. `analyze --html` writes the same page next to its console report.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use heapscope_analysis::leaks::LeakReport;
use heapscope_analysis::Analysis;

use super::analyze;

pub struct ReportArgs {
    pub trace: PathBuf,
    pub output: PathBuf,
    pub suppressions: Option<PathBuf>,
    pub no_builtin_suppressions: bool,
}

pub fn run(args: ReportArgs) -> Result<ExitCode, String> {
    let (analysis, leaks) = analyze::load(
        &args.trace,
        args.suppressions.as_deref(),
        args.no_builtin_suppressions,
    )?;
    write(&analysis, &leaks, &args.output)?;
    Ok(ExitCode::SUCCESS)
}

/// Render the page to `path` and say where it went.
pub fn write(analysis: &Analysis, leaks: &LeakReport, path: &Path) -> Result<(), String> {
    let page = heapscope_report::render(analysis, leaks);
    std::fs::write(path, &page).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    eprintln!(
        "heapscope: wrote report {} ({})",
        path.display(),
        super::bytes(page.len() as u64)
    );
    Ok(())
}
