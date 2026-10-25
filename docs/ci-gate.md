# Gating memory regressions in CI

`heapscope-ci` (paid tier) runs your program under HeapScope in CI, compares the result with a
baseline stored in your repository, and fails the build when memory got worse. It works offline:
no HeapScope server is involved, and the licence is checked locally.

Two ready-made pipelines are in [`ci-templates/`](../ci-templates): GitHub Actions and Azure
Pipelines. The GitHub Actions one has been run end to end (a clean build passes, a seeded regression
fails). **The Azure Pipelines one is written to the same design but has not been run on Azure yet**;
expect to adjust it, and tell us what you had to change. [`examples/ci-demo`](../examples/ci-demo) is a small project with a clean and a
regressed build to try them on.

## What you need

* A 64-bit Windows program with debug info (a `.pdb` next to the exe), built so it exits on its own.
  The program is the **scenario**: a deterministic run (a startup, a load test, one request batch)
  that allocates about the same every time.
* `heapscope.exe` with `heapscope_capture.dll` beside it, and `heapscope-ci.exe`, from the zip files
  you were sent. The templates download both and check their SHA-256.
* A licence key in the CI secret `HEAPSCOPE_LICENSE` (see [Licence key](#licence-key)).

## 1. Make a baseline

Run the scenario on a build you trust and commit the result:

```text
heapscope-ci baseline update --scenario startup --build-flavour release --run --repeat 3 -- build\app.exe --scenario startup
git add heapscope-baselines/startup.hsbaseline.json
```

`heapscope-baselines/<scenario>.hsbaseline.json` holds the peak, the leak count and bytes, total
allocations and bytes, and the top call paths. It is plain JSON, so a baseline change shows up in a
pull request like any other change. Update it the same way whenever a change in memory use is
intended.

Make the baseline with the same kind of build the CI gates (a release CRT against a release CRT).
Debug and release builds allocate differently; `--build-flavour` records which one it was, and the
gate fails with a clear message when the two differ.

## 2. Gate a build

```text
heapscope-ci gate --scenario startup --build-flavour release --run --repeat 3 ^
    --fail-on peak:+5% --fail-on leak_bytes:+64KB --fail-on leaks:+10 ^
    --markdown verdict.md -- build\app.exe --scenario startup
```

Everything after `--` is your program. `--repeat N` runs it N times and compares the median of each
metric, which smooths out run-to-run noise. If you already have a summary
(`heapscope analyze trace.hstrace -o summary.json`), pass `--summary summary.json` instead of
`--run`.

### Rules

`--fail-on METRIC:RULE`, repeatable. A scenario with no `--fail-on` only prints the comparison.

| Metric | Meaning |
|---|---|
| `peak` | highest live bytes at any moment |
| `leaks` | allocations still live at exit (leak candidates) |
| `leak_bytes` | bytes in those allocations |
| `allocs` | total allocation calls |
| `bytes` | total bytes requested |

| Rule | Fails when |
|---|---|
| `+5%`, `+64KB`, `+1MB`, `+10` | the metric grew by **more** than that over the baseline |
| `>N`, `>=N`, `<N`, `<=N`, `==N` | the current value satisfies the comparison, whatever the baseline was |

Sizes are 1024-based (`KB` = KiB); counts take no unit. Growth rules ignore growth under the noise
floor (`--noise-floor-bytes`, default 4096; `--noise-floor-count`, default 0), so a few stray bytes
do not fail a 5% rule on a small program.

**Leaks are rarely zero.** The Windows loader and the C runtime keep a few allocations alive until
exit, so a program with no leaks of its own still reports some (the demo has 25, about 21 KB).
Gate the *growth* of `leaks` and `leak_bytes` against the baseline; `leaks:>0` would fail every
build. Use `>0` only when your baseline was made with a suppressions file that removes them.

### Exit codes

| Code | Meaning |
|---|---|
| 0 | pass |
| 1 | regression: a rule failed |
| 2 | usage or configuration: bad option, no baseline for the scenario, `heapscope` not found |
| 3 | licence: key missing, malformed, wrongly signed or expired |
| 4 | input not accepted: sampled or attached capture, unknown summary version, lost events, the scenario itself failed |

### Where the verdict goes

The Markdown table is printed to the log. It is also appended to the job summary on GitHub
(`$GITHUB_STEP_SUMMARY`), attached to the run on Azure Pipelines, and written to `--markdown FILE`
for a pull-request comment. For a failing metric it names the call path that contributed most.

## Why sampled and attached captures are refused

`--sample N` records one allocation in N, so totals and peaks are estimates; `attach` only sees
allocations made after it connected, so everything allocated earlier looks like it never happened.
A gate that fails builds has to compare exact numbers, so `heapscope-ci` exits 4 on both rather than
risk a false alarm or a missed regression. `--min-lifetime` captures are accepted because the totals
stay exact.

## 3. Put it in CI

Copy a template and fill in the block at the top.

| | GitHub Actions | Azure Pipelines |
|---|---|---|
| File | [`ci-templates/github-actions/heapscope.yml`](../ci-templates/github-actions/heapscope.yml) | [`ci-templates/azure-devops/heapscope.yml`](../ci-templates/azure-devops/heapscope.yml) |
| Runs on | `windows-latest` | `windows-latest` |
| Licence secret | repository secret `HEAPSCOPE_LICENSE` | secret pipeline variable `HEAPSCOPE_LICENSE` |
| Verdict | job summary and a PR comment | the run's Extensions tab |
| Report | artifact `heapscope-report` | pipeline artifact `heapscope-report` |

Both do the same: download `heapscope` and `heapscope-ci` from the URLs you set
(`HEAPSCOPE_URL` / `GATE_URL`) and fail if the SHA-256 does not match, build your program, gate it,
and always publish the HTML report, even when the gate failed, so you can open the call tree of the
regressed build. The gate step's exit code is held back until the report and comment are done, then
the last step fails the job with it.

**Pull requests from forks** do not receive repository secrets, so the gate fails with exit 3
there. The template skips the PR comment for forks for the same reason (the token is read-only).

Azure Pipelines does not hand secret variables to scripts; the template maps `HEAPSCOPE_LICENSE`
into the gate step's `env` for that reason.

## Licence key

The key is a signed text string that carries the licensee, plan, seats and an expiry date. It is
verified on the build machine with a public key built into `heapscope-ci`; nothing is sent anywhere.

* Store it as a CI **secret**, never in the repository or the pipeline file. The templates read it
  only in the gate step.
* A missing, malformed or expired key is a hard failure (exit 3), not a warning. Founding-beta keys
  expire 45 days after they were issued; ask for a fresh one before then.

## Troubleshooting

| Symptom | Likely cause |
|---|---|
| exit 2 `no baseline` | `heapscope-baselines/<scenario>.hsbaseline.json` is not committed, or the job runs in another directory |
| exit 2 `heapscope not found` | `HEAPSCOPE_EXE` is unset and `heapscope.exe` is not on `PATH` |
| exit 3 | the secret is missing or empty (forks, or Azure's explicit mapping), or the key expired |
| exit 4 `scenario failed` | your program returned non-zero under HeapScope; run it by hand with `heapscope run` |
| every function shows as `module+0x...` | no `.pdb` next to the exe in CI |
| a clean build fails `leak_bytes` by a few KB on CI | the runner's C runtime allocates a little more at startup than your PC; use a limit of 16 KB or more, or make the baseline from a CI run |
| peak differs by a lot between runs | the scenario is not deterministic (threads, timers, input); make it so, or raise `--repeat` |
