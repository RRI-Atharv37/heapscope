# HeapScope trace format (`.hstrace`), version 1.1

A trace is a stream of CRC-checked chunks. The reference implementation is the
[`heapscope-trace`](../crates/heapscope-trace) crate (GPL-3.0). Anyone may write a reader or writer
from this document.

All fixed-width integers are little-endian. "varint" is an unsigned LEB128 (at most 10 bytes).
"zigzag" is a varint of `(v << 1) ^ (v >> 63)` for a signed 64-bit value. "string" is a varint
byte length followed by UTF-8 bytes.

## File layout

```
magic        8 bytes   "HSTRACE\0"
major        u16       1
minor        u16       1
chunk...               header chunk, then stacks/events chunks in any order, then a footer chunk
```

A reader must refuse a file whose `major` differs from the one it supports. A higher `minor` with
the same `major` must be readable: minor versions only append fields at the end of the header
chunk, which readers ignore. A new chunk type requires a new `major`. A new event tag may be added in
a minor version only for events a writer produces only when the program needs them; a reader that
predates the tag stops at it as at any unknown tag. History: 1.1 added tag 11.

## Chunk frame (14 bytes + payload)

| Offset | Size | Field |
|---|---|---|
| 0 | u8 | tag: 1 header, 2 stacks, 3 events, 4 footer |
| 1 | u8 | codec: 0 raw, 1 LZ4 block |
| 2 | u32 | raw length (payload size after decompression) |
| 6 | u32 | stored length (bytes that follow) |
| 10 | u32 | CRC-32 (IEEE) of the stored bytes |

Both lengths are limited to 64 MiB. The LZ4 codec is the raw block format without a size prefix;
the raw length is the expected output size. A writer only uses LZ4 when it makes the chunk smaller.
The writer flushes an events chunk after about 256 KiB of raw events.

## Header chunk (first chunk, always raw)

`tool_version` string · `pid` varint · `exe_path` string · `command_line` string · `pointer_size` u8
(4 or 8) · `start_unix_ns` varint (wall-clock time of timestamp 0) · `capture_mode` u8 (0 full; 1
sampled, followed by `one_in` varint, never 0) · `min_lifetime_ms` varint (0 = off) · `attached` u8
(1 = the tool joined a running process, so earlier allocations are missing).

`sampled`, `attached` and a non-zero `min_lifetime_ms` tell analysis that the trace is not a complete
record. Sampled and attached traces must not be used for pass/fail gates.

## Stacks chunk

`count` varint, then `count` stacks: `depth` varint, then `depth` frames as zigzag deltas, the first
relative to 0 and each next relative to the previous frame (innermost frame first). Stack ids are
assigned in order of appearance across the whole file, starting at 1. Id 0 means "no stack". A
stacks chunk always precedes the first events chunk that uses its stacks.

## Events chunk

A sequence of events up to the end of the payload. The timestamp and address deltas restart from 0 at
the start of every chunk, so a chunk decodes without the chunks before it.

Every event starts with `tag` u8, then `ts` as a zigzag delta from the previous event's timestamp
(nanoseconds since capture start), then, except for ChurnSummary, `thread` varint. Addresses are
zigzag deltas from the previous address-carrying field. Events need not be in timestamp order.

| Tag | Event | Fields after the common head |
|---|---|---|
| 1 | Alloc | `kind` u8 (0 heap, 1 pool, 2 virtual) · `addr` · `size` varint (reserved) · `size - requested` zigzag · `stack` varint |
| 2 | Free | `addr` · `stack` |
| 3 | Realloc | `old_addr` · `new_addr` · `old_size` · `new_size` · `new_size - requested` zigzag · `stack` |
| 4 | Marker | `label` string |
| 5 | ModuleLoad | `base` varint · `size` varint · `pdb_guid` 16 bytes · `pdb_age` varint · `path` string |
| 6 | ModuleUnload | `base` varint |
| 7 | ThreadName | `name` string |
| 8 | ChurnSummary | (no thread) `stack` · `allocs` · `bytes` · `total_lifetime_ns` · `max_concurrent`, all varints |
| 9 | HandleOpen | `handle` varint · `kind` u8 (0 file, 1 key, 2 event, 3 mutant, 4 semaphore, 5 section, 6 pipe, other values kept as is) · `stack` |
| 10 | HandleClose | `handle` varint · `stack` |
| 11 | PoolFree | `addr` · `stack` (1.1) |

A **PoolFree** frees an Alloc of kind 1 (pool); a plain Free frees an Alloc of kind 0 or 2. Pool
objects live in their own address space: slot 0 of a pool can sit at the very address of the heap
block that holds the pool, so the two must not be matched by address alone. A trace without pool
allocations never contains a PoolFree.

`requested` is the size the program asked for; `size` is what the allocator reserved. They differ
under the Debug CRT, which adds guard bytes. HandleOpen/HandleClose are reserved for optional
kernel-handle tracking; a capture that does not track handles never writes them.

**ChurnSummary** carries totals for allocations that `min_lifetime_ms` removed from the stream:
neither their Alloc nor their Free is written, and the totals per stack are written instead
(`max_concurrent` is the most dropped allocations of that stack alive at once). Summing
`allocs`, `bytes` and `total_lifetime_ns` over all ChurnSummary events gives exactly what was dropped.

## What the HeapScope capture writes

These are conventions of the capture DLL (S1.4), not requirements of the format:

- **Heap allocations.** The DLL sees calls to `RtlAllocateHeap`, `RtlReAllocateHeap` and
  `RtlFreeHeap`, which only receive the requested size, so `size == requested` for kind 0. The
  Windows heap reports the same number back from `HeapSize`. Under the Debug CRT the requested size
  includes the CRT's own 52-byte header and guards.
- **Frees carry no stack** (`stack = 0`): capturing a stack on every free would cost more than the
  information is worth. Analysis attributes a free to the stack of the allocation it matches.
- **Reallocs** are written as two events, a Free of the old address stamped *before* the real call
  and an Alloc of the new address stamped *after* it. The old block is released inside the call, so
  no allocation another thread makes at that address can sort ahead of the Free; the new block does
  not exist before the call returns, so no free another thread made of that address can sort behind
  the Alloc. (One Realloc event with one timestamp cannot be right for both: the capture used to
  write that, and readers still accept it.) A sampled capture decides each half on its own.
- **Ordering.** Each thread writes into its own buffer, so events from different threads are only
  roughly in time order. Sort by `ts` (stable) before replaying; an event and its matching free can
  share a timestamp only if the clock did not advance between them (the clock ticks every 100 ns).
- **`pdb_guid`** holds the 16 bytes exactly as stored in the module's CodeView (`RSDS`) debug record
  (the little-endian GUID layout), so they can be compared byte for byte with the PDB's identity.
- **Churn totals** are emitted when a thread ends, when capture stops and at exit, possibly several
  times per stack (sum them). `max_concurrent` counts allocations of the stack that were *held for
  the lifetime test* at the same time, so it can slightly exceed the true number: some of the held
  allocations are later written out normally.
- **Sampling** keeps each allocation with probability 1/N, drawn independently per allocation, and
  keeps the free of every kept allocation (matched through a table of kept addresses, whichever
  thread frees it). Multiply counts and bytes of a sampled trace by N for an estimate.
- **Attached captures** (`attached = true` in the header) miss every allocation made before the
  attach, so a Free whose address was never allocated in the trace is normal there; count and ignore it.

## Footer chunk (last chunk, raw)

`event_count` · `stack_count` · `dropped_overflow` · `dropped_min_lifetime` · `dropped_sampling`,
all varints. `dropped_overflow` counts events lost to full capture buffers; the other two count what
the capture options removed on purpose.

A footer exists only if the capture shut down cleanly. A file that ends exactly between chunks
without a footer is a valid trace of a capture that was killed; a file that ends inside a chunk
yields every event before that chunk, then a truncation error. A reader must verify that the footer
counts equal what it read.

## Size

Measured on 1 million synthetic events (500k allocs, 500k frees, 3000 stacks, 4 threads, clustered
addresses): 11.8 MB, **11.8 bytes per event**, against 56 bytes for an uncompressed fixed-size record.
Ten million allocations (20 million events) therefore need about 240 MB. On a synthetic
churn-heavy stream (95 % of allocations freed within 1 ms), `min_lifetime_ms = 1` shrank the file
from 3.4 MB to 0.19 MB. Real traces will differ; this will be re-measured in S1.6.
