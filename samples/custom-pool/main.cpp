// custom-pool: objects come from a pool carved out of one big malloc block.
//   pool_user_a:    200 objects allocated and released (no leak)
//   pool_leak_site:  50 objects allocated and never released (a leak INSIDE the pool)
// usage: custom-pool [--expected file]
#include "../common/expected.hpp"
#include "pool.hpp"

#include <cstdio>

#if defined(_MSC_VER)
#define NOINLINE __declspec(noinline)
#else
#define NOINLINE __attribute__((noinline))
#endif

static void* g_held[256];
static void* volatile g_sink;

NOINLINE static void pool_user_a(FixedPool& pool) {
    void* tmp[200];
    for (int i = 0; i < 200; ++i) { tmp[i] = pool.allocate(); g_sink = tmp[i]; }
    for (int i = 0; i < 200; ++i) pool.release(tmp[i]);
}

NOINLINE static void pool_leak_site(FixedPool& pool) {
    for (int i = 0; i < 50; ++i) { g_held[i] = pool.allocate(); g_sink = g_held[i]; }
}

int main(int argc, char** argv) {
    {
        FixedPool pool(1024);  // 1024 x 64 B = 64 KiB backing block
        pool_user_a(pool);
        pool_leak_site(pool);

        hs_expected::Expected e("custom-pool");
        auto& a = e.site("pool_user_a", "pool");
        a.allocs = 200; a.alloc_bytes = 200ull * 64; a.frees = 200; a.freed_bytes = 200ull * 64;
        auto& b = e.site("pool_leak_site", "pool");
        b.allocs = 50; b.alloc_bytes = 50ull * 64;
        e.write(argc, argv);
    }  // the pool and its backing block go away; the 50 unreleased objects are the leak
    std::printf("custom-pool: 250 pool allocations, 50 never released (pool_leak_site)\n");
    return 0;
}
