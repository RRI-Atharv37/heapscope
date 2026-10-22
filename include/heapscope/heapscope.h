/* SPDX-License-Identifier: MIT
 *
 * heapscope.h - opt-in API for custom allocators and named markers.
 *
 * Tell HeapScope about the allocations your pool hands out:
 *
 *     void* p = pool_take(...);
 *     HEAPSCOPE_ALLOC(p, size, pool);        // after the pool hands out p
 *     ...
 *     HEAPSCOPE_FREE(p, pool);               // before the pool takes p back
 *
 *     HEAPSCOPE_REALLOC(old_p, new_p, new_size, pool);   // pool resized a block
 *     HEAPSCOPE_MARK("level loaded");        // labelled point in time, for diffs
 *
 * When the program is not running under HeapScope every macro costs one cached lookup and does
 * nothing, so it is safe to leave them in release builds. The header needs no library to link
 * against. It works from C and C++ (C89 and later), and from any number of threads and translation
 * units. Windows only for now; on other platforms the macros compile to nothing.
 *
 * `pool` is any pointer that identifies the pool (usually `this`); it is passed along so a later
 * HeapScope version can group allocations per pool.
 */
#ifndef HEAPSCOPE_H
#define HEAPSCOPE_H

#ifdef _WIN32
#include <stddef.h>
#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#include <windows.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef void (*heapscope_alloc_fn)(void* ptr, size_t size, const void* pool);
typedef void (*heapscope_free_fn)(void* ptr, const void* pool);
typedef void (*heapscope_marker_fn)(const char* label);

typedef struct heapscope_api {
    heapscope_alloc_fn on_alloc;
    heapscope_free_fn on_free;
    heapscope_marker_fn marker;
    volatile LONG state; /* 0 = not resolved, 1 = being resolved, 2 = resolved */
} heapscope_api;

/* One table per translation unit (static), resolved at most once per unit. Two threads can race to
 * resolve: the loser spins until the winner has stored all three pointers. */
static __inline heapscope_api* heapscope_get(void) {
    static heapscope_api api;
    if (api.state != 2) {
        if (InterlockedCompareExchange(&api.state, 1, 0) == 0) {
            HMODULE m = GetModuleHandleA("heapscope_capture.dll");
            if (m) {
                api.on_alloc = (heapscope_alloc_fn)(void*)GetProcAddress(m, "heapscope_on_alloc");
                api.on_free = (heapscope_free_fn)(void*)GetProcAddress(m, "heapscope_on_free");
                api.marker = (heapscope_marker_fn)(void*)GetProcAddress(m, "heapscope_marker");
            }
            InterlockedExchange(&api.state, 2);
        } else {
            while (api.state != 2) YieldProcessor();
        }
    }
    return &api;
}

#ifdef __cplusplus
}
#endif

#define HEAPSCOPE_ALLOC(ptr, size, pool) \
    do { heapscope_api* hs_ = heapscope_get(); if (hs_->on_alloc) hs_->on_alloc((ptr), (size), (pool)); } while (0)
#define HEAPSCOPE_FREE(ptr, pool) \
    do { heapscope_api* hs_ = heapscope_get(); if (hs_->on_free) hs_->on_free((ptr), (pool)); } while (0)
/* A resize is recorded as a free of `old_ptr` followed by an allocation of `new_ptr`. Call it after
 * the pool has resized; either pointer may be NULL (NULL is ignored). */
#define HEAPSCOPE_REALLOC(old_ptr, new_ptr, new_size, pool) \
    do { \
        heapscope_api* hs_ = heapscope_get(); \
        if (hs_->on_free) hs_->on_free((old_ptr), (pool)); \
        if (hs_->on_alloc) hs_->on_alloc((new_ptr), (new_size), (pool)); \
    } while (0)
/* `name` is a NUL-terminated string, cut off after 200 bytes. */
#define HEAPSCOPE_MARK(name) \
    do { heapscope_api* hs_ = heapscope_get(); if (hs_->marker) hs_->marker((name)); } while (0)

#else /* not Windows: no-ops */
#define HEAPSCOPE_ALLOC(ptr, size, pool) ((void)0)
#define HEAPSCOPE_FREE(ptr, pool) ((void)0)
#define HEAPSCOPE_REALLOC(old_ptr, new_ptr, new_size, pool) ((void)0)
#define HEAPSCOPE_MARK(name) ((void)0)
#endif

#endif /* HEAPSCOPE_H */
