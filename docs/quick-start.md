# Quick start (draft)

HeapScope records every heap allocation a Windows program makes, then tells you where the memory
went and what was never freed. It works on 64-bit programs, with no source changes.

## 1. Build

You need Rust, the Visual Studio Build Tools with the Windows SDK, and CMake for the samples.

```bash
cargo build --release
cmake -S samples -B build/samples
cmake --build build/samples --config RelWithDebInfo
```

The tool is `target\release\heapscope.exe`.

## 2. Record a run

```bash
heapscope run -o leak.hstrace -- build\samples\RelWithDebInfo\leak.exe
```

Everything after `--` is your program and its arguments. The trace is written to `leak.hstrace`.
Build your own program with debug info (a `.pdb` next to the exe, or on `_NT_SYMBOL_PATH`) or the
report shows `module+0xoffset` instead of function names and lines.

To profile a program that is already running, use `heapscope attach --pid 1234 --duration 30s`;
that shows growth since the attach, not leaks. `--sample N` and `--min-lifetime` lower the
overhead; see [capture options](capture-options.md).

## 3. Read the result

```bash
heapscope analyze leak.hstrace
```

You get the totals and peak, the top allocation sites with source locations, and **leak
candidates**: allocations still live when the program ended, grouped by call path. A candidate is
evidence, not proof; see the [report guide](report-guide.md) for what it means and how to silence
memory that is meant to live until exit (`--suppressions rules.txt`).

## Open it in a browser

```bash
heapscope report leak.hstrace -o report.html
```

`report.html` is one file that opens offline in any browser (`analyze --html report.html` writes it
alongside the console report). The Summary tab has the headline numbers, top call paths and leak
candidates; Call tree lets you expand, sort and search every call path; Flame graph shows the same
tree as boxes you can click to zoom into. A banner above every tab says when the numbers are
estimates or lower bounds (sampled, attached late, short-lived allocations folded, events lost).

## 4. Compare two runs

```bash
heapscope diff before.hstrace after.hstrace
heapscope diff app.hstrace --markers start stop
```

## 5. Use it from scripts

```bash
heapscope analyze leak.hstrace -o summary.json
```

writes a versioned JSON summary, described in [json-schema.md](json-schema.md).
