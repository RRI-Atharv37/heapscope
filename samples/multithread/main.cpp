// multithread: allocations on 8 threads, a cross-thread hand-off, and a per-thread leak.
//   handoff_site:  8 threads x 1000 x 128 B allocated in phase 1, freed by a *different* thread in phase 2
//   mt_leak_site:  8 threads x 100 x 256 B, never freed
// usage: multithread [--expected file]
#include "../common/expected.hpp"

#include <cstdio>
#include <cstdlib>
#include <thread>
#include <vector>

#if defined(_MSC_VER)
#define NOINLINE __declspec(noinline)
#else
#define NOINLINE __attribute__((noinline))
#endif

constexpr int kThreads = 8;
constexpr int kHandoff = 1000;
constexpr int kLeaks = 100;

static void* volatile g_sink;
static void* g_handoff[kThreads][kHandoff];
static void* g_leaked[kThreads][kLeaks];

NOINLINE static void handoff_site(int t) {
    for (int i = 0; i < kHandoff; ++i) { g_handoff[t][i] = ::operator new(128); g_sink = g_handoff[t][i]; }
}

NOINLINE static void mt_leak_site(int t) {
    for (int i = 0; i < kLeaks; ++i) { g_leaked[t][i] = std::malloc(256); g_sink = g_leaked[t][i]; }
}

int main(int argc, char** argv) {
    {
        std::vector<std::thread> phase1;
        for (int t = 0; t < kThreads; ++t) phase1.emplace_back([t] { handoff_site(t); mt_leak_site(t); });
        for (auto& th : phase1) th.join();
    }
    {
        std::vector<std::thread> phase2;
        for (int t = 0; t < kThreads; ++t)
            phase2.emplace_back([t] {
                int victim = (t + 1) % kThreads;  // free another thread's blocks
                for (int i = 0; i < kHandoff; ++i) ::operator delete(g_handoff[victim][i]);
            });
        for (auto& th : phase2) th.join();
    }

    hs_expected::Expected e("multithread");
    auto& a = e.site("handoff_site", "operator new");
    a.allocs = kThreads * kHandoff; a.alloc_bytes = 128ull * kThreads * kHandoff;
    a.frees = a.allocs; a.freed_bytes = a.alloc_bytes;
    auto& b = e.site("mt_leak_site", "malloc");
    b.allocs = kThreads * kLeaks; b.alloc_bytes = 256ull * kThreads * kLeaks;
    e.write(argc, argv);

    std::printf("multithread: %d threads, %d handed-off blocks each, %d leaked each\n", kThreads, kHandoff, kLeaks);
    return 0;
}
