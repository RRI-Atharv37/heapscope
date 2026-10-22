//! `heapscope` command-line entry point. Subcommands are stubs until the capture core lands (S1.4).

use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "heapscope", version, about = "Windows heap profiler")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Launch a program and record its heap activity into a trace file
    Run {
        /// Output trace file
        #[arg(short, long, default_value = "out.hstrace")]
        output: PathBuf,
        /// Program to run, followed by its arguments
        #[arg(trailing_var_arg = true, required = true)]
        program: Vec<String>,
    },
    /// Summarise a trace file and list leak candidates
    Analyze {
        /// Trace file to read
        trace: PathBuf,
    },
    /// Compare two trace files
    Diff {
        /// Baseline trace
        before: PathBuf,
        /// Trace to compare against the baseline
        after: PathBuf,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let name = match cli.command {
        Command::Run { .. } => "run",
        Command::Analyze { .. } => "analyze",
        Command::Diff { .. } => "diff",
    };
    eprintln!("heapscope {name}: not implemented yet");
    ExitCode::from(2)
}
