// private-heap: a HeapCreate arena that is released with HeapDestroy, without freeing its blocks
// one by one (a common Windows idiom). The destroy frees every block.
//   arena_site: 100 x 777 B allocated from the private heap, all released by HeapDestroy
// usage: private-heap [--expected file]
#include "../common/expected.hpp"

#include <cstdio>
#include <windows.h>

#if defined(_MSC_VER)
#define NOINLINE __declspec(noinline)
#else
#define NOINLINE __attribute__((noinline))
#endif

constexpr int kBlocks = 100;
static void* volatile g_sink;

NOINLINE static void arena_site(HANDLE arena) {
    for (int i = 0; i < kBlocks; ++i) g_sink = HeapAlloc(arena, 0, 777);
}

int main(int argc, char** argv) {
    HANDLE arena = HeapCreate(0, 0, 0);
    arena_site(arena);
    HeapDestroy(arena);

    hs_expected::Expected e("private-heap");
    auto& a = e.site("arena_site", "HeapAlloc");
    a.allocs = kBlocks; a.alloc_bytes = 777ull * kBlocks;
    a.frees = kBlocks; a.freed_bytes = 777ull * kBlocks;
    e.write(argc, argv);

    std::printf("private-heap: %d blocks released by HeapDestroy\n", kBlocks);
    return 0;
}
