# Changelog

## 0.1.0-beta

First beta, for the founding users.

### Capture
- `heapscope run -- app.exe` records every heap allocation of a 64-bit Windows program into an
  `.hstrace` file, with no source changes; `heapscope attach --pid N` does it for a running process.
- Covers the C and C++ runtime and `HeapAlloc`, with dynamic or static CRT. `VirtualAlloc` and
  kernel handles are not recorded yet. Custom allocators can report through the optional
  MIT-licensed header in `include/`.
- `--sample N` and `--min-lifetime` lower the overhead; the report says when numbers are estimates.
- Frees made while a thread or the process shuts down are now recorded, so per-thread loader
  blocks and memory a DLL frees in its static destructors no longer show up as live or as missed frees.
- An exception raised out of a heap call (`HEAP_GENERATE_EXCEPTIONS`) no longer stops recording on that thread.
- After `attach` ends, the per-thread capture buffers are released instead of staying committed.
- Module paths longer than 260 characters are kept in full, and dropped module events are counted.

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
  licensed offline. Templates for GitHub Actions (tested) and Azure Pipelines (not yet run on Azure) and a demo project are in
  `ci-templates/` and `examples/ci-demo/`; see `docs/ci-gate.md`.

### Known limits
- Windows x64 only; MSVC-built programs are the tested case.
- Sampled and attached captures cannot be used with the CI gate.
- Unsigned binaries: Windows 11 Smart App Control may block them.
- The Azure Pipelines template has not been run on Azure yet.
- Not yet tested on large real-world applications.
