# HeapScope

A heap profiler for native Windows programs: record allocations, find leaks, diff two runs, and
(in the paid tier) gate memory regressions in CI.

**Status:** pre-alpha. The workspace builds and the `heapscope` CLI has stubbed subcommands only;
capture, the trace format and the report are not implemented yet.

## Build

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

## Layout

| Path | What |
|---|---|
| `crates/heapscope-cli` | the `heapscope` command-line tool |
| `crates/heapscope-capture` | `heapscope_capture.dll`, injected into the profiled process |
| `include/heapscope` | optional C/C++ header for custom allocators |
| `samples` | small C++ programs with known allocation behaviour |

## Licence

GPL-3.0-only, see [LICENSE](LICENSE).
