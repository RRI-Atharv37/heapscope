// growth: memory grows in phases, then partly shrinks. Peak is after phase 2; phase 1 is released in phase 3.
//   phase 1: +4 MiB (64 x 64 KiB)   phase 2: +8 MiB (128 x 64 KiB)   phase 3: free phase 1
// usage: growth [--expected file]
#include "../common/expected.hpp"

#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <thread>

#if defined(_MSC_VER)
#define NOINLINE __declspec(noinline)
#else
#define NOINLINE __attribute__((noinline))
#endif

constexpr unsigned kBlock = 64 * 1024;
static void* g_phase1[64];
static void* g_phase2[128];

NOINLINE static void grow_phase_1() { for (auto& p : g_phase1) { p = std::malloc(kBlock); } }
NOINLINE static void grow_phase_2() { for (auto& p : g_phase2) { p = std::malloc(kBlock); } }
NOINLINE static void shrink_phase_3() { for (auto& p : g_phase1) { std::free(p); p = nullptr; } }

static void pause_ms(int ms) { std::this_thread::sleep_for(std::chrono::milliseconds(ms)); }

int main(int argc, char** argv) {
    grow_phase_1(); pause_ms(50);
    grow_phase_2(); pause_ms(50);
    shrink_phase_3(); pause_ms(50);

    hs_expected::Expected e("growth");
    auto& a = e.site("grow_phase_1", "malloc");
    a.allocs = 64; a.alloc_bytes = 64ull * kBlock; a.frees = 64; a.freed_bytes = 64ull * kBlock;  // freed in phase 3
    auto& b = e.site("grow_phase_2", "malloc");
    b.allocs = 128; b.alloc_bytes = 128ull * kBlock;                                               // live at exit
    e.write(argc, argv);

    std::printf("growth: peak ~12 MiB after phase 2, ~8 MiB live at exit\n");
    return 0;
}
