# FAQ

## Does profiling make my program GPL?

No. HeapScope is a separate program that observes yours from outside; your program is not a
derivative of it and keeps its own licence. The only file you may copy into your code is
`include/heapscope/heapscope.h` (for [custom allocators](custom-allocators.md)), and it is MIT
licensed. The rest of HeapScope is GPL-3.0-only, which applies if you modify and distribute
HeapScope itself.

## Does it send my data anywhere?

No. Capture, analysis and reports all run on your machine, and the HTML report is one offline file.
The CI gate checks its licence locally.

Resolving symbols reads the program images named in the trace from your local disk (the same paths the
capture recorded). Paths that are not plain local drive paths, such as network shares or device names, are
never opened, so a trace from someone else cannot make `analyze` contact another machine. The report
carries a content security policy that forbids all network access.

## Which programs does it work on?

64-bit Windows programs that use the Windows heap, which includes `malloc`, `new` and `HeapAlloc`
from the MSVC runtime, dynamic or static. MSVC-built programs are the tested case. 32-bit programs,
Linux and macOS are not supported. Memory taken straight from `VirtualAlloc` and kernel handles is
not recorded yet; see [capture options](capture-options.md#not-captured).

## Can I use it on games or programs with anti-cheat?

No. HeapScope injects a DLL and hooks system functions, which anti-cheat treats as tampering, and
you may get banned. Profile offline builds, or your own tools and servers.

## <a id="sample-or-min-lifetime"></a>When do I use `--sample` and when `--min-lifetime`?

| Situation | Use | Cost |
|---|---|---|
| Lots of short-lived allocations (`new`/`delete` churn) | `--min-lifetime 5ms` | Totals stay exact; short-lived blocks are counted per call stack instead of traced |
| Allocation rate so high that the program is too slow or the trace too big | `--sample N` | Counts become estimates (multiply by N); rare sites can be missed |
| Looking for a leak | neither if you can afford it | Full capture is the most trustworthy |
| Gating CI | neither | The gate rejects sampled, attached and `--min-lifetime` captures: it needs an exact peak |

Both are recorded in the trace and shown in the report banner.

## Why not UMDH, the CRT debug heap, Dr. Memory or WonderLeak?

- **UMDH** (Windows debugging tools) takes two heap snapshots and diffs them. It needs gflags
  setup and a console workflow, and gives text only, with no timeline, call tree or reports.
- **CRT debug heap** (`_CrtDumpMemoryLeaks`) needs a Debug build and recompilation, reports leaks
  with a source line only for the allocation call, and has no call stacks by default.
- **Dr. Memory** is a thorough checker, but it runs the program under binary instrumentation,
  which is very slow, and its 64-bit Windows support is limited.
- **WonderLeak** is a commercial leak finder with its own workflow.

HeapScope's niche is: no source changes, any release build with a PDB, an attach mode for programs
that are already running, a call tree with a timeline, and a CI gate. If a tool above already solves
your problem, use it. We have not benchmarked these tools against each other; this list is about
what each one is designed for, so check their current docs.

## Does MSVC AddressSanitizer find leaks?

No. AddressSanitizer on Windows finds memory *errors* (overflows, use after free); LeakSanitizer is
not supported on Windows. Use HeapScope or the CRT debug heap for leaks.

## Is a "leak candidate" really a leak?

Not always. It is memory still live when the program ended. Caches and singletons that live until
exit are candidates too. See the [report guide](report-guide.md#leak-candidates) and use
suppressions for the ones you know are fine.

## Why do I see leaks in a program that has none?

The loader and C runtime keep some allocations until exit, and `leaks:>0` in the CI gate would fail
every build. See [troubleshooting](troubleshooting.md#numbers-differ-from-what-i-expected).

## Can I attach to a running program and find leaks?

Not "leaks at exit"; you get growth since the attach and differences between markers. See
[troubleshooting](troubleshooting.md#why-does-attach-not-show-leaks-at-exit).

## How do I compare two builds?

`heapscope diff before.hstrace after.hstrace`. Sites are matched by function name, not address, so
rebuilds still line up. See the [report guide](report-guide.md#snapshot-diff).
