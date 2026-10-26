// growth: memory grows in phases, then partly shrinks. Peak is after phase 2; phase 1 is released in phase 3.
//   phase 1: +4 MiB (64 x 64 KiB)   phase 2: +8 MiB (128 x 64 KiB)   phase 3: free phase 1
// usage: growth [--expected file]
//        growth --wait-for-attach      (for `heapscope attach`: prints "PID <n>", then obeys stdin lines)
//            grow N    allocate N more 64 KiB blocks, keep them, reply "grown N"
//            shrink N  free N of the blocks allocated by `grow`, reply "shrunk N"
//            mem       reply "private <bytes>": the process's private (committed) memory
//            mark TEXT reply "marked": HEAPSCOPE_MARK("TEXT") from the program itself (heapscope.h)
//            quit      exit with code 0
#include "../../include/heapscope/heapscope.h"
#include "../common/expected.hpp"

#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <string>
#include <thread>
#include <windows.h>
#include <psapi.h>

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

static void* g_attach_blocks[4096];
static unsigned g_attach_count = 0;

NOINLINE static void attach_grow(unsigned n) {
    for (unsigned i = 0; i < n && g_attach_count < 4096; ++i) g_attach_blocks[g_attach_count++] = std::malloc(kBlock);
}

NOINLINE static void attach_shrink(unsigned n) {
    for (unsigned i = 0; i < n && g_attach_count > 0; ++i) std::free(g_attach_blocks[--g_attach_count]);
}

static int wait_for_attach() {
    std::printf("PID %lu\n", (unsigned long)GetCurrentProcessId());
    std::fflush(stdout);
    char line[128];
    while (std::fgets(line, sizeof line, stdin)) {
        unsigned n = 0;
        if (std::sscanf(line, "grow %u", &n) == 1) { attach_grow(n); std::printf("grown %u\n", n); }
        else if (std::sscanf(line, "shrink %u", &n) == 1) { attach_shrink(n); std::printf("shrunk %u\n", n); }
        else if (std::strncmp(line, "mem", 3) == 0) {
            PROCESS_MEMORY_COUNTERS_EX pmc = {};
            GetProcessMemoryInfo(GetCurrentProcess(), reinterpret_cast<PROCESS_MEMORY_COUNTERS*>(&pmc), sizeof pmc);
            std::printf("private %llu\n", (unsigned long long)pmc.PrivateUsage);
        }
        else if (std::strncmp(line, "mark ", 5) == 0) {
            std::string label(line + 5);
            while (!label.empty() && (label.back() == '\n' || label.back() == '\r')) label.pop_back();
            HEAPSCOPE_MARK(label.c_str());
            std::printf("marked\n");
        }
        else if (std::strncmp(line, "quit", 4) == 0) break;
        std::fflush(stdout);
    }
    return 0;
}

int main(int argc, char** argv) {
    for (int i = 1; i < argc; ++i)
        if (std::strcmp(argv[i], "--wait-for-attach") == 0) return wait_for_attach();

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
