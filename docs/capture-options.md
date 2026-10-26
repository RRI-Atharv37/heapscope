# Capture options

`heapscope run` starts a program under HeapScope. `heapscope attach` joins a program that is already
running. Both write a `.hstrace` file ([format](trace-format.md)).

## run

```
heapscope run -o out.hstrace [--sample N] [--min-lifetime 5ms] [--stack-depth N] -- app.exe args
```

HeapScope creates the program suspended, loads `heapscope_capture.dll` into it, then lets it start,
so every allocation from its first instruction is recorded. The program keeps its console, working
directory, environment and exit code (`heapscope run` exits with the program's code, or 1 if that
code does not fit in a byte). The trace is closed when the program exits normally; if the program is
killed or crashes, the trace is still readable up to the last quarter second but has no footer.

The DLL must sit next to `heapscope.exe`, or be named in the `HEAPSCOPE_DLL` environment variable.
`HEAPSCOPE_DLL` must be a **full path to an existing file** (`C:\tools\heapscope_capture.dll`): the
path is passed to `LoadLibrary` inside the program being profiled, which would look for a relative
name in its own folders first, so a relative or missing path is refused. If the DLL comes from a
different HeapScope build than `heapscope.exe`, the error says which two control-block versions
differ; use the DLL that was shipped with the executable.

## attach

```
heapscope attach --pid 1234 -o out.hstrace [--duration 30s] [--sample N] [--min-lifetime 5ms]
```

HeapScope loads the DLL into the running process, records until the duration ends or you press
Ctrl-C, then tells the DLL to flush and go **dormant**: its hooks pass straight through and the
program keeps running normally. The DLL is never unloaded from a live process, so it stays in the
program's memory until the program exits. The capture buffers (2 MiB per thread that allocated, and
the tables behind `--sample` and `--min-lifetime`) are given back when the capture ends; what stays is the DLL itself and one 4 KiB page per such thread. A process can be captured **once per
lifetime**; a second attach is refused with an explanation.

Each line you type on `attach`'s standard input becomes a **marker** in the trace (an empty line is
named `marker 1`, `marker 2`, ...), confirmed with `marker recorded: <label>`. Markers are how you
compare phases: type one, do the thing in the program, type another. `--until-eof` also stops the
capture when standard input is closed, which is what scripts want.

What an attached trace means:

- Only allocations made **after** the attach are seen. A free of an allocation made earlier is
  recorded but has no matching allocation. The numbers are *net growth since attach* and
  *differences between markers*, not "what leaked at exit".
- The header says `attached`, and `Header::is_partial()` is true, so the CI gate (S2) rejects it.
- Threads that already existed work like any other; the first allocation on each creates its buffer.

Limits:

| Situation | What happens |
|---|---|
| 32-bit target | Refused: HeapScope captures 64-bit processes only |
| Target runs elevated or protected, or belongs to another user | Refused with the Windows error; try an elevated prompt |
| Anti-cheat or endpoint protection that blocks remote threads | Injection fails with "cannot start a thread in the target" |
| DLL blocked (Smart App Control, antivirus) | `LoadLibrary failed in the target` or the DLL's own reason |
| The target already has the DLL | Refused, see above |

## Keeping traces small

A program that allocates millions of times per second produces a lot of events. Two options trade
completeness for size. Both are recorded in the header, and **both make the trace partial**, which the
CI gate rejects: use them for exploration, not for gating.

### `--min-lifetime <duration>` (for example `5ms`, `250ms`)

An allocation freed on the same thread within this time is not written at all. Instead, the stack's
**ChurnSummary** counts it: how many, how many bytes, total lifetime, and the most held at once. Nothing
is lost from the totals: add the summaries to the allocations still in the trace and you get exactly
what the program did. Allocations that live longer than the threshold are written with their original
timestamp, a little later. An allocation freed by a *different* thread is written normally: its free
is in the other thread's part of the trace, and the allocation is written when its hold ends. A
free from any thread cancels the hold on that address, so a stale hold can never absorb the free of
a later allocation that happens to reuse the address (programs that pass blocks between threads
reuse addresses constantly). Up to about 110,000 allocations can be held at once across all
threads (the table is 2 MiB, given back when the capture ends); beyond that, new ones are written
normally.

This is what makes `new`/`delete` churn cheap to record: on the `churn` sample (50,000 short-lived
allocations) the trace shrinks several times over while the totals stay exact.

### `--sample N`

Keep one allocation in N. Each allocation is drawn independently (a pseudo-random draw per thread),
and a kept allocation is remembered in a table of kept addresses so that its free, from whichever
thread, is kept too: the live set of the kept allocations is exact. The header stores N. Multiply
counts and bytes by N for an estimate.

What the estimate is worth:

- It is unbiased, including for sites that reuse a few addresses over and over (a hot
  `new`/`delete` pair). The error of a site is about `1 / sqrt(allocations kept)`: a site with
  50,000 allocations at `--sample 16` keeps about 3,100 and lands within a few percent of the
  truth. A site with fewer than a few times N allocations is noisy or absent.
- Live bytes at the end, the peak and leak candidates are estimates of the same kind: a leaked
  block is in the trace with probability 1/N, so a small leak can be missed entirely, and a
  sampled trace can never show that a program is leak free.
- The table of kept addresses holds about 230,000 live kept allocations (about 2.3 million live
  allocations at `--sample 10`) in 4 MiB that is given back when the capture ends. If it fills up,
  further sampled allocations are not recorded and are counted under `dropped.overflow`, which
  marks the capture as lossy.

Sampling also removes most of the per-allocation cost of capturing a stack, so it is the option to
use when overhead matters more than detail.

### `--stack-depth N`

Frames recorded per call stack, 1 to 64 (default 32). Deeper stacks cost more per allocation.

## What each number means

- **Sizes are what the program asked the heap for.** Under the Debug CRT that includes the CRT's own
  52 bytes per block, so Debug and Release builds of the same code differ.
- A counter in the summary line, `events were lost`, means the program allocated faster than the trace
  could be written and the buffers stayed full for 20 ms; those events are gone. It does not happen in
  normal use; it appeared only under a deliberately extreme loop in a debug build. Use `--sample` or
  `--min-lifetime` for such programs.
- The `run`/`attach` summary line reads from the DLL's counters, so it is correct even if the target
  was killed before the trace was closed.

## Private heaps

`HeapDestroy` frees every block of the heap without calling `HeapFree` for each. HeapScope walks the
heap just before it is destroyed and records a free for every block still allocated, so an arena
created with `HeapCreate` and released with `HeapDestroy` leaves no leak candidates. (A destroy that
fails would still have been recorded as freed.)

## Not captured

Custom pools that never call the heap (see `include/heapscope`), `VirtualAlloc`/`mmap`-style
allocations, and kernel handles are not recorded yet. Child processes are not followed.
