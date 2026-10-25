# Changelog

## 0.1.0-beta

First beta, for the founding users.

### Capture
- `heapscope run -- app.exe` records every heap allocation of a 64-bit Windows program into an
  `.hstrace` file, with no source changes; `heapscope attach --pid N` does it for a running process.
- Covers the C and C++ runtime, `HeapAlloc`, `VirtualAlloc` and kernel handles, with dynamic or
  static CRT. Custom allocators can report through the optional MIT-licensed header in `include/`.
- `--sample N` and `--min-lifetime` lower the overhead; the report says when numbers are estimates.

### Analysis
- `heapscope analyze`: totals, peak, top allocation sites with source lines, and leak candidates
  grouped by call path, with built-in and user suppressions.
- `heapscope diff` compares two traces or two markers in one trace.
- `analyze -o summary.json` writes a versioned JSON summary (schema v1, see `docs/json-schema.md`).

### Reports
- `heapscope report` writes one offline HTML file: summary, call tree, flame graph, timeline with
  markers, and a diff view.

### CI gate (paid tier)
- `heapscope-ci` fails a build on a memory regression against a baseline in your repository,
  licensed offline. Templates for GitHub Actions and Azure Pipelines and a demo project are in
  `ci-templates/` and `examples/ci-demo/`; see `docs/ci-gate.md`.

### Known limits
- Windows x64 only; MSVC-built programs are the tested case.
- Sampled and attached captures cannot be used with the CI gate.
- Unsigned binaries: Windows 11 Smart App Control may block them.
- Not yet tested on large real-world applications.
