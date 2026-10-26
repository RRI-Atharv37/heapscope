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
 * When the program is not running under HeapScope every macro costs one tick-count read and does
 * nothing (the lookup for the capture DLL is repeated about four times a second until it succeeds,
 * so a later `heapscope attach` is picked up), so it is safe to leave them in release builds. The
 * header needs no library to link against and does not include <windows.h>, define any macro or
 * leak any name other than the `heapscope_` and `HEAPSCOPE_` ones. It works from C and C++ (C89
 * and later), and from any number of threads and translation units. Windows x64 only for now; on
 * other platforms the macros compile to nothing. Every macro argument is evaluated at most once.
 *
 * Under `heapscope attach`, allocations made before the attach are invisible, and so is the free of
 * such an allocation.
 *
 * `pool` is any pointer that identifies the pool (usually `this`); it is passed along so a later
 * HeapScope version can group allocations per pool.
 */
#ifndef HEAPSCOPE_H
#define HEAPSCOPE_H

#ifdef _WIN32
#include <stddef.h>
#include <intrin.h>
#include <emmintrin.h>

#ifdef __cplusplus
extern "C" {
#endif

/* The three kernel32 functions the header needs, declared exactly as <windows.h> declares them so
 * that the two can be included in either order. */
struct HINSTANCE__;
typedef __int64 (__stdcall* heapscope_farproc)();
__declspec(dllimport) struct HINSTANCE__* __stdcall GetModuleHandleA(const char* module_name);
__declspec(dllimport) heapscope_farproc __stdcall GetProcAddress(struct HINSTANCE__* module, const char* name);
__declspec(dllimport) unsigned long __stdcall GetTickCount(void);

typedef void (*heapscope_alloc_fn)(void* ptr, size_t size, const void* pool);
typedef void (*heapscope_free_fn)(void* ptr, const void* pool);
typedef void (*heapscope_marker_fn)(const char* label);

typedef struct heapscope_api {
    heapscope_alloc_fn on_alloc;
    heapscope_free_fn on_free;
    heapscope_marker_fn marker;
    volatile long state; /* 0 = not resolved, 1 = being resolved, 2 = resolved */
    unsigned long last_try; /* tick count of the last lookup that found no DLL */
    long tried;
} heapscope_api;

/* One table per translation unit (static). Only success is remembered: while the capture DLL is not
 * loaded, the lookup is retried once every 250 ms, so a `heapscope attach` made later is noticed.
 * Two threads can race to resolve: the loser waits until the winner has stored all three pointers. */
static __inline heapscope_api* heapscope_get(void) {
    static heapscope_api api;
    if (api.state != 2) {
        if (api.state == 1) {
            while (api.state == 1) _mm_pause();
        } else {
            unsigned long now = GetTickCount();
            if (!api.tried || (unsigned long)(now - api.last_try) >= 250) {
                if (_InterlockedCompareExchange(&api.state, 1, 0) == 0) {
                    struct HINSTANCE__* m = GetModuleHandleA("heapscope_capture.dll");
                    if (m) {
                        api.on_alloc = (heapscope_alloc_fn)(void*)GetProcAddress(m, "heapscope_on_alloc");
                        api.on_free = (heapscope_free_fn)(void*)GetProcAddress(m, "heapscope_on_free");
                        api.marker = (heapscope_marker_fn)(void*)GetProcAddress(m, "heapscope_marker");
                        _InterlockedExchange(&api.state, 2);
                    } else {
                        api.last_try = now;
                        api.tried = 1;
                        _InterlockedExchange(&api.state, 0);
                    }
                }
            }
        }
    }
    return &api;
}

#ifdef __cplusplus
}
#endif

#define HEAPSCOPE_ALLOC(ptr, size, pool) \
    do { \
        heapscope_api* heapscope_api_ = heapscope_get(); \
        if (heapscope_api_->on_alloc) heapscope_api_->on_alloc((ptr), (size), (pool)); \
    } while (0)
#define HEAPSCOPE_FREE(ptr, pool) \
    do { \
        heapscope_api* heapscope_api_ = heapscope_get(); \
        if (heapscope_api_->on_free) heapscope_api_->on_free((ptr), (pool)); \
    } while (0)
/* A resize is recorded as a free of `old_ptr` followed by an allocation of `new_ptr`. Call it after
 * the pool has resized; either pointer may be NULL (NULL is ignored). */
#define HEAPSCOPE_REALLOC(old_ptr, new_ptr, new_size, pool) \
    do { \
        heapscope_api* heapscope_api_ = heapscope_get(); \
        const void* heapscope_pool_ = (pool); \
        if (heapscope_api_->on_free) heapscope_api_->on_free((old_ptr), heapscope_pool_); \
        if (heapscope_api_->on_alloc) heapscope_api_->on_alloc((new_ptr), (new_size), heapscope_pool_); \
    } while (0)
/* `name` is a NUL-terminated string, cut off after 200 bytes. */
#define HEAPSCOPE_MARK(name) \
    do { \
        heapscope_api* heapscope_api_ = heapscope_get(); \
        if (heapscope_api_->marker) heapscope_api_->marker((name)); \
    } while (0)

#else /* not Windows: no-ops */
#define HEAPSCOPE_ALLOC(ptr, size, pool) ((void)0)
#define HEAPSCOPE_FREE(ptr, pool) ((void)0)
#define HEAPSCOPE_REALLOC(old_ptr, new_ptr, new_size, pool) ((void)0)
#define HEAPSCOPE_MARK(name) ((void)0)
#endif

#endif /* HEAPSCOPE_H */
