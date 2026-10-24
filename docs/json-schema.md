# Summary JSON schema, version 1

`heapscope analyze trace.hstrace -o summary.json` writes this file (`-o -` writes it to standard
output). **Version 1 is frozen**: a later release may add fields only under a new
`schema_version`; v1 fields are never renamed, removed or changed in meaning. Read
`schema_version` first and refuse a number you do not know. Key order is stable.

Conventions:

- Byte and allocation counts are unsigned integers. For a sampled capture they are already
  multiplied by `capture.sample_rate`, so they are **estimates**; check `capture.estimated`.
  Counts of kernel handles and `unmatched_frees`, `missed_frees` and `dropped` are never scaled.
- `ts_ns` is nanoseconds since the capture began.
- Lists are ordered: `sites` and `leaks.entries` by bytes, largest first, ties broken by name.
- `id` identifies a call path across runs: the module and function of its innermost 12 frames
  (`module!function <- module!function ...`). It contains no addresses or line numbers.
- `null` stands for an unknown module, file or similar.

## Top level

| Field | Type | Meaning |
|---|---|---|
| `schema_version` | integer | Always `1` |
| `tool` | string | Always `"heapscope"` |
| `capture` | object | How the trace was captured |
| `totals` | object | Whole-trace counts |
| `peak` | object | The moment the live heap was largest |
| `sites` | array | Every allocation site |
| `leaks` | object | Leak candidates |
| `churn` | array | Short-lived allocation sites |
| `markers` | array | Markers and the live heap at each |
| `warnings` | array of strings | Things that make the numbers less exact |

## `capture`

| Field | Type | Meaning |
|---|---|---|
| `mode` | string | `full`, `sampled` or `attached` |
| `sample_rate` | integer | 1 in N allocations kept; 1 when not sampled |
| `min_lifetime_ms` | integer | `--min-lifetime` used, 0 if none |
| `attached` | boolean | Captured with `attach`; earlier allocations are unknown |
| `estimated` | boolean | `true` for `sampled` and `attached`: counts are estimates, not exact |
| `complete` | boolean | The capture shut down cleanly |
| `exe`, `command_line` | string | The profiled program |
| `pid`, `pointer_size` | integer | |
| `dropped` | object | `overflow`, `min_lifetime`, `sampling`: events not in the trace and why. Only `overflow` is loss |

## `totals`

`allocs`, `alloc_bytes`, `frees`, `freed_bytes`, `live_allocs`, `live_bytes` (live at the end),
`unmatched_frees` (frees of memory the trace never saw allocated), `missed_frees` (allocations at
an address still live, so a free was lost).

## `peak`

`ts_ns`, `live_bytes`, `live_allocs` at the moment of the largest live heap.

## `sites[]`

One per call path that allocated. Allocator and C runtime frames are hidden, so the site is the
first user frame.

| Field | Meaning |
|---|---|
| `id` | Stable call-path identity (see above) |
| `function`, `module`, `file`, `line` | The site; `module+0xoffset` when unresolved |
| `path` | Function names from the site outwards, at most 16 |
| `allocs`, `alloc_bytes`, `frees`, `freed_bytes` | Totals for the site |
| `live_allocs`, `live_bytes` | Still allocated at the end of the trace |
| `peak_live_bytes` | The site's share of the live heap at the global peak |
| `short_lived_allocs` | Allocations freed within the short-lived threshold |

## `leaks`

| Field | Meaning |
|---|---|
| `kind` | `leaks` (live at exit) or `net_growth` (attached capture) |
| `title`, `notes` | Text for people; do not parse |
| `estimated` | Sampled capture: figures are scaled |
| `total_allocs`, `total_bytes` | Of `entries` |
| `suppressed_allocs`, `suppressed_bytes` | Of `suppressed` |
| `by_kind` | `[{kind, allocs, bytes}]`; `kind` is `heap`, `pool`, `virtual` or `handle` |
| `entries` | Candidates, largest first |
| `suppressed` | Entries a suppression rule set aside, with the rule that matched |

An entry has the site fields `id`, `function`, `module`, `file`, `line`, `path`, plus `kind`,
`allocs`, `bytes`, `oldest_ts_ns` and `newest_ts_ns`. A suppressed entry also has
`suppressed_by: {rule, reason, builtin}`. Handles have `allocs` and `bytes` 0.

## `churn[]`

The site fields plus `allocs`, `bytes` (short-lived only), `avg_lifetime_ns`, `max_concurrent`
(only known for `--min-lifetime` captures) and `share` (fraction of the site's allocations that
were short-lived, 0.0 to 1.0).

## `markers[]`

`label`, `ts_ns`, `live_bytes`, `live_allocs` at that marker.

## Reading it safely

A consumer that gates on this file should refuse or flag `capture.estimated` and
`capture.complete == false`, and treat `dropped.overflow > 0` as lower bounds.
