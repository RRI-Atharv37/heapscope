/* SPDX-License-Identifier: MIT
 *
 * heapscope.h - opt-in API for custom allocators.
 * (S0.6 spike version: Windows only, two macros. Completed in S1.5: markers, C docs, tests.)
 *
 * Tell HeapScope about the allocations your pool hands out. When the program is not running
 * under HeapScope the macros do nothing (one cached lookup). Works from C and C++.
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

typedef struct heapscope_api {
    heapscope_alloc_fn on_alloc;
    heapscope_free_fn on_free;
    int resolved;
} heapscope_api;

static __inline heapscope_api* heapscope_get(void) {
    static heapscope_api api;
    if (!api.resolved) {
        HMODULE m = GetModuleHandleA("heapscope_capture.dll");
        if (m) {
            api.on_alloc = (heapscope_alloc_fn)(void*)GetProcAddress(m, "heapscope_on_alloc");
            api.on_free = (heapscope_free_fn)(void*)GetProcAddress(m, "heapscope_on_free");
        }
        api.resolved = 1;
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

#else /* not Windows: no-ops */
#define HEAPSCOPE_ALLOC(ptr, size, pool) ((void)0)
#define HEAPSCOPE_FREE(ptr, pool) ((void)0)
#endif

#endif /* HEAPSCOPE_H */
