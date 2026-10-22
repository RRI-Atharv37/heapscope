// churn: many short-lived allocations from one site, plus a few that survive.
//   churn_site: 50,000 x 48 B in batches of 64, each batch freed straight away (lifetime << 1 ms)
//   survivor_site: 20 x 4096 B, held until exit
// Used to check `--min-lifetime`: the churn must shrink the trace, and what was folded into the
// per-stack totals plus what was still written must add up to the truth.
// usage: churn [--expected file]
#include "../common/expected.hpp"

#include <cstdio>
#include <cstdlib>

#if defined(_MSC_VER)
#define NOINLINE __declspec(noinline)
#else
#define NOINLINE __attribute__((noinline))
#endif

static void* volatile g_sink;
static void* g_survivors[20];

NOINLINE static void churn_site() {
    void* batch[64];
    for (int round = 0; round < 50000 / 64 + 1; ++round) {
        int n = (round == 50000 / 64) ? 50000 % 64 : 64;
        for (int i = 0; i < n; ++i) { batch[i] = ::operator new(48); g_sink = batch[i]; }
        for (int i = 0; i < n; ++i) ::operator delete(batch[i]);
    }
}

NOINLINE static void survivor_site() {
    for (auto& p : g_survivors) { p = std::malloc(4096); g_sink = p; }
}

int main(int argc, char** argv) {
    churn_site();
    survivor_site();

    hs_expected::Expected e("churn");
    auto& a = e.site("churn_site", "operator new");
    a.allocs = 50000; a.alloc_bytes = 48ull * 50000; a.frees = 50000; a.freed_bytes = 48ull * 50000;
    auto& b = e.site("survivor_site", "malloc");
    b.allocs = 20; b.alloc_bytes = 20ull * 4096;
    e.write(argc, argv);

    std::printf("churn: 50000 short-lived x 48 B, 20 survivors x 4 KiB\n");
    return 0;
}
