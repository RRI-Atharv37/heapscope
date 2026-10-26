/* Exception-safe calls of the original ntdll heap functions (BUG-3).
 *
 * A heap created with HEAP_GENERATE_EXCEPTIONS, or a call with that flag, raises an SEH exception
 * out of the allocator instead of returning NULL. Rust has no SEH, so the hooks call the originals
 * through these wrappers; when an exception unwinds through one, `hs_on_unwind` puts the thread's
 * reentrancy guard and busy flag back. The exception itself keeps propagating to the program. */
#include <windows.h>

extern void hs_on_unwind(void);

typedef PVOID(NTAPI* alloc_fn)(PVOID, ULONG, SIZE_T);
typedef PVOID(NTAPI* realloc_fn)(PVOID, ULONG, PVOID, SIZE_T);
typedef BOOLEAN(NTAPI* free_fn)(PVOID, ULONG, PVOID);

PVOID hs_call_alloc(alloc_fn f, PVOID heap, ULONG flags, SIZE_T size) {
    PVOID r = NULL;
    __try {
        r = f(heap, flags, size);
    } __finally {
        if (AbnormalTermination()) hs_on_unwind();
    }
    return r;
}

PVOID hs_call_realloc(realloc_fn f, PVOID heap, ULONG flags, PVOID old, SIZE_T size) {
    PVOID r = NULL;
    __try {
        r = f(heap, flags, old, size);
    } __finally {
        if (AbnormalTermination()) hs_on_unwind();
    }
    return r;
}

BOOLEAN hs_call_free(free_fn f, PVOID heap, ULONG flags, PVOID p) {
    BOOLEAN r = 0;
    __try {
        r = f(heap, flags, p);
    } __finally {
        if (AbnormalTermination()) hs_on_unwind();
    }
    return r;
}
