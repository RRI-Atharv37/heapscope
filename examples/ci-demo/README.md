# ci-demo

A tiny program for trying the [HeapScope CI gate](../../docs/ci-gate.md) and the
[CI templates](../../ci-templates). It runs one scenario, `startup`, in two builds:

| Build | What it does | Gate |
|---|---|---|
| clean | builds a 200-entry cache, serves 50 requests, frees everything | passes |
| `-DDEMO_REGRESSION=ON` | the cache is 600 entries and every request leaks a 1 KiB buffer | fails: peak +204%, 50 more leaks |

`heapscope-baselines/startup.hsbaseline.json` is the baseline of the clean build (release CRT,
RelWithDebInfo).

## Try it locally

You need `heapscope.exe` (with `heapscope_capture.dll`), `heapscope-ci.exe` and a licence key in
`HEAPSCOPE_LICENSE`. In a Visual Studio developer prompt:

```text
cmake -S . -B build\clean -G Ninja -DCMAKE_BUILD_TYPE=RelWithDebInfo
cmake --build build\clean
cmake -S . -B build\regressed -G Ninja -DCMAKE_BUILD_TYPE=RelWithDebInfo -DDEMO_REGRESSION=ON
cmake --build build\regressed

set RULES=--fail-on peak:+10% --fail-on leak_bytes:+4KB --fail-on leaks:+10
heapscope-ci gate --scenario startup --build-flavour release --run --repeat 3 %RULES% -- build\clean\ci-demo.exe
heapscope-ci gate --scenario startup --build-flavour release --run --repeat 3 %RULES% -- build\regressed\ci-demo.exe
```

The first exits 0, the second exits 1 and names `handle_request` (the leak) and `build_cache` (the
bigger peak). Regenerate the baseline after changing `src/main.cpp`:

```text
heapscope-ci baseline update --scenario startup --build-flavour release --run --repeat 3 -- build\clean\ci-demo.exe
```

## In CI

Copy a template, point `APP` at `build/ci-demo.exe` (or the `build/RelWithDebInfo/` path of your
generator), set `-DDEMO_REGRESSION=ON` in the Build step to see the gate fail.
