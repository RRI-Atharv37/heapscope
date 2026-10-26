// heap-exception: a heap created with HEAP_GENERATE_EXCEPTIONS raises STATUS_NO_MEMORY when an
// allocation fails, and programs that use the flag catch it with __try/__except. The exception
// leaves the heap call through the profiler's hook, which must not stop recording on this thread.
//   after_exception_site: 50 x 3333 B allocated after three caught exceptions, never freed
// usage: heap-exception [--expected file]
#include "../common/expected.hpp"

#include <cstdio>
#include <cstdlib>
#include <windows.h>

#if defined(_MSC_VER)
#define NOINLINE __declspec(noinline)
#else
#define NOINLINE __attribute__((noinline))
#endif

constexpr int kAfter = 50;
static void* volatile g_sink;
static void* g_kept[kAfter];

// Kept free of C++ objects so __try is allowed.
NOINLINE static int alloc_too_much(HANDLE heap) {
    __try {
        g_sink = HeapAlloc(heap, HEAP_GENERATE_EXCEPTIONS, 64u << 20);
        return 0;
    } __except (GetExceptionCode() == STATUS_NO_MEMORY ? EXCEPTION_EXECUTE_HANDLER : EXCEPTION_CONTINUE_SEARCH) {
        return 1;
    }
}

NOINLINE static void after_exception_site() {
    for (auto& p : g_kept) { p = std::malloc(3333); g_sink = p; }
}

int main(int argc, char** argv) {
    HANDLE heap = HeapCreate(HEAP_GENERATE_EXCEPTIONS, 64 * 1024, 1024 * 1024);  // fixed size: 1 MiB
    int caught = 0;
    for (int i = 0; i < 3; ++i) caught += alloc_too_much(heap);
    after_exception_site();

    hs_expected::Expected e("heap-exception");
    auto& a = e.site("after_exception_site", "malloc");
    a.allocs = kAfter; a.alloc_bytes = 3333ull * kAfter;
    e.write(argc, argv);

    std::printf("heap-exception: %d exceptions caught, %d blocks allocated afterwards\n", caught, kAfter);
    return caught == 3 ? 0 : 2;
}
