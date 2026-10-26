# Troubleshooting

## The program or DLL will not start

**"Application Control policy has blocked this file"** (or the file just does not run). Windows 11
Smart App Control blocks unsigned executables and DLLs, and it can do so at random: a build that ran
a minute ago may be refused after a rebuild, and the first load of `heapscope_capture.dll` can take
a second or two while Windows checks it. This is the policy, not a HeapScope bug. Options:

- Rebuild or re-download the file and try again.
- Turn Smart App Control off (Windows Security, App & browser control). It cannot be turned back on
  without resetting Windows, so decide deliberately.
- Use a machine that does not have it enabled.

Releases will be signed; until then, expect this on new Windows 11 PCs.

**Antivirus or endpoint protection.** HeapScope injects a DLL into the program and hooks `ntdll`
functions, which is also what malware does. If a scanner quarantines `heapscope.exe` or
`heapscope_capture.dll`, or `run` fails with `LoadLibrary failed in the target`, add an exclusion for
the folder that holds them.

**`heapscope_capture.dll` not found.** It must sit next to `heapscope.exe`, or be named by its full
path in the `HEAPSCOPE_DLL` environment variable (a relative path is refused).

**"the capture DLL belongs to a different HeapScope build".** `heapscope.exe` and
`heapscope_capture.dll` come from different versions and cannot talk to each other. Use the pair
from one download, and check that `HEAPSCOPE_DLL` is not pointing at an old copy.

## Function names are missing

Frames shown as `module+0x1a2b` mean no symbols were found for that module. HeapScope resolves them
after capture from `.pdb` files:

- Build your program with debug info (`/Zi` and `/DEBUG`; Release builds can have it too) and keep
  the `.pdb` next to the exe.
- For Windows and third-party DLLs, set `_NT_SYMBOL_PATH`, for example
  `srv*C:\symbols*https://msdl.microsoft.com/download/symbols`. The first analysis downloads them.
- Analyse on the machine that has the binaries and PDBs. The trace records module paths and build
  ids, not symbols.

Unresolved frames are not an error; the report still works and diffs of the same build still match.

## Admin rights

`heapscope run` needs none for a program you start yourself. `attach` can only open processes
you may open: a target that is elevated, protected, or owned by another user is refused with the
Windows error. Run `heapscope` from an elevated prompt for an elevated target. Protected
processes (anti-malware services, some system services) cannot be attached at all.

## `attach` limits

| You see | Why |
|---|---|
| refused: 32-bit process | HeapScope captures 64-bit processes only |
| refused: access denied | target is elevated, protected, or another user's; try an elevated prompt |
| `cannot start a thread in the target` | anti-cheat or endpoint protection blocks remote threads. Do not attach to programs protected by anti-cheat; it may get your account banned |
| refused: already captured | a process can be captured once per lifetime; restart it |

See [capture options](capture-options.md#attach) for the full list.

## "Why does attach not show leaks at exit?"

Because it did not see the program start. Only allocations made after the attach are tracked, so
memory allocated earlier looks like it never existed. An attached trace answers "what grew since I
attached, and between my markers", not "what is still live at exit". The report is titled
accordingly. To find leaks, use `heapscope run`, so the capture sees every allocation.

## The profiled program crashes or exits early

- The program keeps its own exit code; `heapscope run` exits with it. Check the console output of
  the program first.
- If the program is killed or crashes, the trace is still readable up to the last quarter second but
  has no footer, and the report says the capture did not shut down cleanly. "Live at exit" then
  means "live where the trace stops".
- A crash inside `heapscope_capture.dll` itself is a bug: please report it with the program, the
  command line and the Windows version.
- Programs that start child processes: only the first process is captured.

## Numbers differ from what I expected

- **Debug CRT adds 52 bytes per block.** Under the Debug CRT every block carries a header and guard
  bytes, so Debug and Release builds of the same code report different sizes. Compare like with like.
- **A clean program still shows leak candidates.** The loader and the C runtime keep a few
  allocations (tens of KB) alive until exit. The built-in suppressions hide the known ones; hide
  more with `--suppressions`. See the [report guide](report-guide.md#suppressions).
- **Sizes are what the program asked for**, not what the heap reserved, so they will not match
  Task Manager or the process working set.
- **Events lost** in the summary means the program allocated faster than the trace could be written.
  Re-run with `--sample` or `--min-lifetime`.
- **Numbers are labelled estimated** when the trace was sampled; see [FAQ](faq.md#sample-or-min-lifetime).

## Very large traces or a slow program

HeapScope records a call stack for every allocation, so allocation-heavy programs slow down. Pick
the option that fits ([FAQ](faq.md#sample-or-min-lifetime)): `--min-lifetime` for churn,
`--sample N` when overhead matters most, a lower `--stack-depth` when stacks are deep.
