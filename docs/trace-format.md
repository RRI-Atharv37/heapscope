# HeapScope trace format (`.hstrace`), version 1.0

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
minor        u16       0
chunk...               header chunk, then stacks/events chunks in any order, then a footer chunk
```

A reader must refuse a file whose `major` differs from the one it supports. A higher `minor` with
the same `major` must be readable: minor versions only append fields at the end of the header
chunk, which readers ignore. New event tags or new chunk types require a new `major`.

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

`requested` is the size the program asked for; `size` is what the allocator reserved. They differ
under the Debug CRT, which adds guard bytes. HandleOpen/HandleClose are reserved for optional
kernel-handle tracking; a capture that does not track handles never writes them.

**ChurnSummary** carries totals for allocations that `min_lifetime_ms` removed from the stream:
neither their Alloc nor their Free is written, and the totals per stack are written instead
(`max_concurrent` is the most dropped allocations of that stack alive at once). Summing
`allocs`, `bytes` and `total_lifetime_ns` over all ChurnSummary events gives exactly what was dropped.

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
