//! `heapscope` command-line entry point. `run` and `attach` capture; `diff` compares live memory;
//! `analyze` is a stub until S2.5.

mod commands;
mod launch;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Args, Parser, Subcommand};

#[derive(Parser)]
#[command(name = "heapscope", version, about = "Windows heap profiler")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// Options that decide how much a capture records.
#[derive(Args)]
struct CaptureArgs {
    /// Output trace file
    #[arg(short, long, default_value = "out.hstrace")]
    output: PathBuf,
    /// Keep one allocation in N (chosen by address, so its free is kept too). The CI gate rejects sampled traces
    #[arg(long, value_name = "N", default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..))]
    sample: u32,
    /// Fold allocations freed sooner than this (for example 5ms) into per-stack totals instead of tracing them
    #[arg(long, value_name = "DURATION", value_parser = parse_min_lifetime)]
    min_lifetime: Option<u32>,
    /// Frames to record per call stack (default 32, at most 64)
    #[arg(long, value_name = "N", default_value_t = 0, value_parser = clap::value_parser!(u32).range(0..=64))]
    stack_depth: u32,
}

fn parse_min_lifetime(s: &str) -> Result<u32, String> {
    let ms = commands::parse_duration_ms(s, 1)?;
    u32::try_from(ms).map_err(|_| "too long".to_string())
}

fn parse_duration(s: &str) -> Result<Duration, String> {
    commands::parse_duration_ms(s, 1000).map(Duration::from_millis)
}

#[derive(Subcommand)]
enum Command {
    /// Launch a program and record its heap activity into a trace file
    Run {
        #[command(flatten)]
        capture: CaptureArgs,
        /// Program to run, followed by its arguments
        #[arg(trailing_var_arg = true, required = true)]
        program: Vec<String>,
    },
    /// Record the heap activity of an already running 64-bit process for a while, then detach
    Attach {
        /// Process id to attach to
        #[arg(long)]
        pid: u32,
        #[command(flatten)]
        capture: CaptureArgs,
        /// Stop after this long, for example 30s or 2m (default: until Ctrl-C)
        #[arg(long, value_parser = parse_duration)]
        duration: Option<Duration>,
        /// Also stop when standard input is closed (for scripts that drive markers through stdin)
        #[arg(long)]
        until_eof: bool,
    },
    /// Summarise a trace file and list leak candidates
    Analyze {
        /// Trace file to read
        trace: PathBuf,
    },
    /// Compare the live memory of two trace files, or of two markers in one trace
    Diff {
        /// Baseline trace
        before: PathBuf,
        /// Trace to compare against the baseline (omit with --markers)
        after: Option<PathBuf>,
        /// Compare two markers of the baseline trace instead; '@end' is the end of the trace
        #[arg(long, num_args = 2, value_names = ["FROM", "TO"])]
        markers: Option<Vec<String>>,
        /// Sites to list (0 for all)
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Hide sites whose live bytes changed by less than this
        #[arg(long, value_name = "BYTES", default_value_t = 0)]
        min_change: u64,
        /// Frames from the allocation site that identify a call path across runs
        #[arg(long, value_name = "N", default_value_t = 12, value_parser = clap::value_parser!(usize))]
        depth: usize,
        /// Also list sites that did not change
        #[arg(long)]
        all: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Run { capture, program } => commands::run::run(commands::run::RunArgs {
            output: capture.output,
            program,
            sample_one_in: capture.sample,
            min_lifetime_ms: capture.min_lifetime.unwrap_or(0),
            stack_depth: capture.stack_depth,
        }),
        Command::Attach {
            pid,
            capture,
            duration,
            until_eof,
        } => commands::attach::attach(commands::attach::AttachArgs {
            pid,
            output: capture.output,
            duration,
            stop_on_eof: until_eof,
            sample_one_in: capture.sample,
            min_lifetime_ms: capture.min_lifetime.unwrap_or(0),
            stack_depth: capture.stack_depth,
        }),
        Command::Analyze { .. } => not_implemented("analyze"),
        Command::Diff {
            before,
            after,
            markers,
            limit,
            min_change,
            depth,
            all,
        } => commands::diff::run(commands::diff::DiffArgs {
            before,
            after,
            markers: markers.and_then(|m| {
                let mut it = m.into_iter();
                Some((it.next()?, it.next()?))
            }),
            limit,
            min_change,
            depth,
            include_unchanged: all,
        }),
    };
    match result {
        Ok(code) => code,
        Err(message) => {
            eprintln!("heapscope: {message}");
            ExitCode::from(1)
        }
    }
}

fn not_implemented(name: &str) -> Result<ExitCode, String> {
    eprintln!("heapscope {name}: not implemented yet");
    Ok(ExitCode::from(2))
}
