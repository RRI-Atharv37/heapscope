// leak: deliberate leaks at two known call sites, plus one balanced site that must NOT be reported.
// usage: leak [--expected file]
#include "../common/expected.hpp"

#include <cstdio>
#include <cstdlib>

#if defined(_MSC_VER)
#define NOINLINE __declspec(noinline)
#else
#define NOINLINE __attribute__((noinline))
#endif

static void* volatile g_sink;
static void* g_leaked[256];  // keeps the pointers reachable: the leak is "still reachable", not lost

// LEAK 1: 100 x 1024 bytes via operator new, never freed
NOINLINE static void leak_site_a() {
    for (int i = 0; i < 100; ++i) { g_leaked[i] = ::operator new(1024); g_sink = g_leaked[i]; }
}

// LEAK 2: 10 x 65536 bytes via malloc, never freed
NOINLINE static void leak_site_b() {
    for (int i = 0; i < 10; ++i) { g_leaked[100 + i] = std::malloc(65536); g_sink = g_leaked[100 + i]; }
}

// NOT a leak: 100 x 512 bytes, freed immediately
NOINLINE static void balanced_site() {
    for (int i = 0; i < 100; ++i) { void* p = ::operator new(512); g_sink = p; ::operator delete(p); }
}

int main(int argc, char** argv) {
    leak_site_a();
    leak_site_b();
    balanced_site();

    hs_expected::Expected e("leak");
    auto& a = e.site("leak_site_a", "operator new");
    a.allocs = 100; a.alloc_bytes = 100ull * 1024;
    auto& b = e.site("leak_site_b", "malloc");
    b.allocs = 10; b.alloc_bytes = 10ull * 65536;
    auto& c = e.site("balanced_site", "operator new");
    c.allocs = 100; c.alloc_bytes = 100ull * 512; c.frees = 100; c.freed_bytes = 100ull * 512;
    e.write(argc, argv);

    std::printf("leak: leaked 100 x 1 KiB at leak_site_a and 10 x 64 KiB at leak_site_b\n");
    return 0;
}
