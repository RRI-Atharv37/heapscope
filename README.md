# HeapScope

A heap profiler for native Windows programs: record allocations, find leaks, diff two runs, and
(in the paid tier) gate memory regressions in CI.

**Status:** `0.1.0-beta`, see the [changelog](CHANGELOG.md). `heapscope run` and `heapscope attach`
record allocations into a trace file ([format](docs/trace-format.md), [options](docs/capture-options.md));
`analyze`, `diff` and `report` read it (a self-contained HTML page with summary, call tree, flame
graph, timeline and diff view). The paid `heapscope-ci` gate is described in [docs/ci-gate.md](docs/ci-gate.md).

## Install

Unpack `heapscope-0.1.0-beta.zip` anywhere and put the folder on `PATH`. `heapscope.exe` and
`heapscope_capture.dll` must stay together. Then follow the [quick start](docs/quick-start.md).
Questions and feedback: GitHub Discussions on this repository.

## Docs

[Quick start](docs/quick-start.md) · [capture options](docs/capture-options.md) ·
[report guide](docs/report-guide.md) · [custom allocators](docs/custom-allocators.md) ·
[JSON summary](docs/json-schema.md) · [CI gate](docs/ci-gate.md) · [trace format](docs/trace-format.md)

## Build from source

Requirements: Windows 10/11 x64, Rust stable (MSVC toolchain, pinned by `rust-toolchain.toml`),
Visual Studio Build Tools with the Windows SDK (for the version resource), and CMake for the samples.

```
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

The sample programs used for testing are built separately:

```
cmake -S samples -B build/samples
cmake --build build/samples --config RelWithDebInfo
```

## Try it

```
heapscope run -o leak.hstrace -- build\samples\RelWithDebInfo\leak.exe
heapscope attach --pid 1234 -o growth.hstrace --duration 30s
```

`heapscope_capture.dll` must be next to `heapscope.exe`. Windows 11 Smart App Control may block unsigned
local builds; see [capture options](docs/capture-options.md#attach) for what a block looks like.

## Layout

| Path | What |
|---|---|
| `crates/heapscope-cli` | the `heapscope` command-line tool |
| `crates/heapscope-capture` | `heapscope_capture.dll`, injected into the profiled process |
| `crates/heapscope-trace` | the `.hstrace` format: writer and streaming reader |
| `crates/heapscope-control` | the control block the CLI and the injected DLL share |
| `include/heapscope` | optional MIT-licensed C/C++ header for custom allocators, see [custom allocators](docs/custom-allocators.md) |
| `ci-templates` | GitHub Actions and Azure Pipelines templates for the CI gate |
| `examples/ci-demo` | a small project with a clean and a regressed build, for trying the gate |
| `samples` | small C++ programs with known allocation behaviour |

## Licence

GPL-3.0-only, see [LICENSE](LICENSE). The header in `include/heapscope` is MIT, see
[LICENSE-MIT](include/heapscope/LICENSE-MIT).
