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
- `--sample N` now decides per allocation instead of per address, so the estimate for a site that
  reuses a few addresses (a hot `new`/`delete` pair) is no longer off by a factor of two or more.
  The error is about `1 / sqrt(allocations kept)`; see `docs/capture-options.md`.
- `--min-lifetime` no longer folds a free into a stale hold when blocks change hands between
  threads: a free from any thread cancels the hold on that address, so such programs no longer show
  false leaks.
- A `realloc` is written as a Free of the old address and an Alloc of the new one, each stamped on
  the right side of the call, so it can no longer sort behind or ahead of another thread's free.
- The optional header `heapscope.h` works after `heapscope attach` (it looks for the DLL again
  until it finds it), no longer includes `<windows.h>` or defines `WIN32_LEAN_AND_MEAN`, evaluates
  every argument at most once, and is tested as C89, C11 and C++.
- `HEAPSCOPE_DLL` must be a full path to an existing file. A DLL from another HeapScope build is
  reported as such instead of being blamed on antivirus.
- Frees made while a thread or the process shuts down are now recorded, so per-thread loader
  blocks and memory a DLL frees in its static destructors no longer show up as live or as missed frees.
- An exception raised out of a heap call (`HEAP_GENERATE_EXCEPTIONS`) no longer stops recording on that thread.
- After `attach` ends, the per-thread capture buffers are released instead of staying committed.
- Module paths longer than 260 characters are kept in full, and dropped module events are counted.
- Blocks of a private heap that the program releases with `HeapDestroy` are recorded as freed, so
  they no longer show up as leaks or inflate the peak.
- A pool object reported through the C API can no longer displace the heap block that holds it
  (slot 0 of a pool sits at its backing block's address): the block stays live until it is freed,
  and its pool objects are counted on top of it. The trace format is now 1.1 (a `PoolFree` event);
  1.0 traces still read as before.
- The binaries and the capture DLL carry the crate version in their version resource, and the
  toolchain is pinned in `rust-toolchain.toml`.
- If the `heapscope` that controls a capture ends in any way (killed, closed console, closed output pipe),
  the capture DLL notices and finishes the trace on its own; `run` puts the target in a job object so
  it ends with the CLI. `attach` no longer panics when its output is closed.
- `run` resolves the program itself: a bare name is looked up on `PATH` only (not in the current
  directory), and `.bat`/`.cmd` files are refused instead of being handed to `cmd.exe`.
- Whether the DLL started is decided by the control block, not by the remote thread's exit code.
- A second capture into a trace file that is already being written fails with a clear message.
- `attach` says "exited" when the target ended, no longer reports unconfirmed markers as recorded,
  and a disk-full write error is no longer described as a crash. The control block is now version 2.

### Analysis
- `heapscope analyze`: totals, peak, top allocation sites with source lines, and leak candidates
  grouped by call path, with built-in and user suppressions.
- `heapscope diff` compares two traces or two markers in one trace.
- `analyze -o summary.json` writes a versioned JSON summary (schema v1, see `docs/json-schema.md`).
- A trace is treated as untrusted input: module paths that are not plain local drive paths (network
  shares, device names) are never opened for symbols, the reader caps stacks, frames, events and
  decoded bytes over the whole file, and byte totals saturate with a warning instead of wrapping.

### Reports
- `heapscope report` writes one offline HTML file: summary, call tree, flame graph, timeline with
  markers, and a diff view.
  The page carries a content security policy that forbids all network access and any script but
  its own.

### CI gate (paid tier)
- `heapscope-ci` fails a build on a memory regression against a baseline in your repository,
  licensed offline. Templates for GitHub Actions and Azure Pipelines and a demo project are in
  `ci-templates/` and `examples/ci-demo/`; see `docs/ci-gate.md`. The GitHub template ran on the
  demo as a manual workflow; its pull-request path and the Azure template have not run on a real
  pipeline yet.
- The gate refuses `--min-lifetime` captures as well as sampled and attached ones (their peak is
  understated), names the site that grew most for each metric, passes `--suppressions` to
  `heapscope analyze` with `--run`, keeps the licence key out of the profiled program's
  environment, and with no `--fail-on` prints the comparison and exits 0. Baselines made by an
  earlier build must be made again (`heapscope-ci baseline update`).
- A paid licence key keeps working for 14 days after it expires, with a warning; beta keys do not.

### Known limits
- Windows x64 only; MSVC-built programs are the tested case.
- Sampled and attached captures cannot be used with the CI gate.
- Unsigned binaries: Windows 11 Smart App Control may block them.
- The Azure Pipelines template has not been run on Azure yet.
- Not yet tested on large real-world applications.
