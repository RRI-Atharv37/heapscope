# Contributing

Thanks for helping. Bug reports with a small reproducing program or a trace are the most useful
contribution.

## Build and test

Windows x64 only. You need Rust (MSVC toolchain, pinned to an exact version by `rust-toolchain.toml`), the Visual
Studio Build Tools with the Windows SDK, CMake, and Node 22 if you touch `report/`.

```
cargo build --workspace --locked
cargo test --workspace --locked
cmake -S samples -B build/samples
cmake --build build/samples --config RelWithDebInfo
```

Build the workspace before testing (the capture tests need the DLL), and build the samples first
or the capture and end-to-end tests skip (`HEAPSCOPE_REQUIRE_SAMPLES=1` makes that a failure). Windows
11 Smart App Control can block fresh unsigned builds at random; that is not a code bug.

## Style

```
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo deny check
```

For the report front end: `cd report`, `npm ci`, `npm run check`, `npm run build`. The built
`crates/heapscope-report/assets/report-template.html` is committed; include it whenever `report/src`
changes, because CI compares it with a fresh build.

The frozen summary JSON (`docs/json-schema.md`) is a public contract; do not change it without a
version bump. Keep changes small and add a test or a sample program with known behaviour.

## Pull requests

1. Open an issue first for anything bigger than a small fix.
2. Branch from `main`, keep one topic per PR, add a line to `CHANGELOG.md` for user-visible changes.
3. Sign off every commit (`git commit -s`) to certify the
   [Developer Certificate of Origin](https://developercertificate.org/). Your contribution is then
   licensed under GPL-3.0-only (the header in `include/heapscope` is MIT).
4. CI must pass.

## Conduct and security

See [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md). Report vulnerabilities privately as described in
[SECURITY.md](SECURITY.md).
