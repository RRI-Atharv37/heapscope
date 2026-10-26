# Custom allocators

HeapScope records what a program asks the Windows heap for. A pool, arena or free-list allocator that
carves many objects out of one big block looks like a single allocation, so its leaks and growth are
invisible. `include/heapscope/heapscope.h` lets the pool report each object it hands out, with a few
lines of code. The header is MIT-licensed (`include/heapscope/LICENSE-MIT`) so it can go into any
code base; the rest of HeapScope is GPL-3.0-only.

## Steps

1. Copy `heapscope.h` into your project (or add `include/` to your include path). There is nothing to
   link: the header looks up `heapscope_capture.dll` at run time.
2. `#include "heapscope/heapscope.h"` in the file that implements the pool.
3. Call `HEAPSCOPE_ALLOC(ptr, size, pool)` right after the pool hands out `ptr`, and
   `HEAPSCOPE_FREE(ptr, pool)` right before it takes `ptr` back. `pool` is any pointer that identifies
   the pool, normally `this`.
4. If the pool can resize a block, call `HEAPSCOPE_REALLOC(old_ptr, new_ptr, new_size, pool)` after it.
5. Optionally call `HEAPSCOPE_MARK("label")` at points you want to compare (end of level load, after a
   request). Markers show up in the trace as labelled points in time.
6. Run the program under `heapscope run` or `heapscope attach` as usual.

## Before and after

Before, from `samples/custom-pool/pool.hpp` without the opt-in:

```cpp
void* allocate() {
    void* p;
    if (free_list_) { p = free_list_; free_list_ = *static_cast<void**>(p); }
    else if (next_ < slots_) { p = block_ + (next_++) * kSlot; }
    else return nullptr;
    return p;
}
void release(void* p) {
    *static_cast<void**>(p) = free_list_;
    free_list_ = p;
}
```

HeapScope sees one 64 KiB `malloc` made in the constructor.

After:

```cpp
void* allocate() {
    ...
    HEAPSCOPE_ALLOC(p, kSlot, this);   // added
    return p;
}
void release(void* p) {
    HEAPSCOPE_FREE(p, this);           // added
    ...
}
```

Now every pool object appears with the call path of the code that asked the pool for it, and the 50
objects `pool_leak_site` never releases are listed as live at exit.

## Behaviour to know

- Outside HeapScope the macros do nothing but a cached lookup of the DLL (done once per translation
  unit, safe from any thread). They can stay in release builds.
- The macros obey the same rules as the heap hooks: `--sample` and `--min-lifetime` apply, and a call
  made from inside a hook is ignored.
- Pointers must be unique among live pool objects. Report the pointer the caller receives, not an
  internal one. A pool object may sit at the address of the heap block that holds the pool (slot 0
  of a fixed pool): the two are tracked apart, a pool free never touches the block, and the block
  stays live until it is freed. Pool bytes are added on top of the bytes of the block that holds
  them, so `peak` and live totals count that memory twice; the call tree shows the block under the
  pool's own allocation site and each object under the site that allocated it.
- The macros are no-ops on platforms other than Windows (HeapScope captures on Windows only for now).
- `HEAPSCOPE_REALLOC` is recorded as a free of the old pointer followed by an allocation of the new
  one, so an object that is resized in place shows up as a new allocation at the resize site.
- A label passed to `HEAPSCOPE_MARK` is cut off after 200 bytes.
- Plain C works: `samples/header-c` compiles the header as C and is part of the tests.
