// basic-alloc: a known number of allocations of known sizes, all freed. Nothing leaks.
// usage: basic-alloc [count] [--expected file]   (default count = 10000)
#include "../common/expected.hpp"

#include <chrono>
#include <cstdio>
#include <cstdlib>

#if defined(_MSC_VER)
#define NOINLINE __declspec(noinline)
#else
#define NOINLINE __attribute__((noinline))
#endif

static void* volatile g_sink;  // defeats allocation elision

// 32-byte objects via operator new
NOINLINE static void alloc_small_blocks(unsigned n, void** keep) {
    for (unsigned i = 0; i < n; ++i) { keep[i] = ::operator new(32); g_sink = keep[i]; }
}

// 4096-byte blocks via malloc
NOINLINE static void alloc_large_blocks(unsigned n, void** keep) {
    for (unsigned i = 0; i < n; ++i) { keep[i] = std::malloc(4096); g_sink = keep[i]; }
}

int main(int argc, char** argv) {
    unsigned n = (argc > 1 && argv[1][0] != '-') ? (unsigned)std::atoi(argv[1]) : 10000;
    unsigned large = n / 10 ? n / 10 : 1;
    void** small_ptrs = (void**)std::malloc(sizeof(void*) * n);
    void** large_ptrs = (void**)std::malloc(sizeof(void*) * large);

    auto t0 = std::chrono::steady_clock::now();
    alloc_small_blocks(n, small_ptrs);
    alloc_large_blocks(large, large_ptrs);
    for (unsigned i = 0; i < n; ++i) ::operator delete(small_ptrs[i]);
    for (unsigned i = 0; i < large; ++i) std::free(large_ptrs[i]);
    double ms = std::chrono::duration<double, std::milli>(std::chrono::steady_clock::now() - t0).count();

    hs_expected::Expected e("basic-alloc");
    auto& a = e.site("alloc_small_blocks", "operator new");
    a.allocs = n; a.alloc_bytes = 32ull * n; a.frees = n; a.freed_bytes = 32ull * n;
    auto& b = e.site("alloc_large_blocks", "malloc");
    b.allocs = large; b.alloc_bytes = 4096ull * large; b.frees = large; b.freed_bytes = 4096ull * large;
    e.write(argc, argv);

    std::free(small_ptrs);
    std::free(large_ptrs);
    std::printf("basic-alloc: %u small + %u large allocations, all freed, %.2f ms\n", n, large, ms);
    return 0;
}
