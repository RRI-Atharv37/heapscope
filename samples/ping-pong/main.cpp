// ping-pong: blocks that change hands between threads, and a block that is resized while other
// threads allocate and free the same sizes. Ground truth for the two capture paths where the order
// of events from different threads matters (`--min-lifetime` and realloc).
//   ping_site:    thread A allocates 20,000 x 64 B; thread B frees each one
//   pong_site:    thread B allocates 20,000 x 64 B; thread A frees each one
//   pong_spare_site: thread B allocates one more 64 B block at the start and frees it at the end
//   ping_pong_leak_site: thread B allocates 10 x 96 B and never frees them
//   realloc_initial_site: the 1 MiB block that is resized
//   realloc_stress_site: one thread resizes that block 2,000 times between 1 MiB and 3 MiB
//   contender_site:      two threads allocate and free a 1 MiB block 2,000 times each, meanwhile
// Same-size blocks handed back and forth reuse each other's addresses, which is what makes a hold
// placed for one allocation meet the free of another.
// usage: ping-pong [--expected file]
#include "../common/expected.hpp"

#include <atomic>
#include <cstdio>
#include <cstdlib>
#include <thread>

#if defined(_MSC_VER)
#define NOINLINE __declspec(noinline)
#else
#define NOINLINE __attribute__((noinline))
#endif

constexpr int kRounds = 20000;
constexpr int kLeaks = 10;
constexpr int kResizes = 2000;
constexpr int kContenders = 2;
constexpr size_t kMiB = 1u << 20;

static void* volatile g_sink;
static std::atomic<void*> g_to_b{nullptr};
static std::atomic<void*> g_to_a{nullptr};
static void* g_leaked[kLeaks];

static void* receive(std::atomic<void*>& slot) {
    void* p;
    while ((p = slot.exchange(nullptr)) == nullptr) std::this_thread::yield();
    return p;
}

NOINLINE static void* ping_site() { void* p = ::operator new(64); g_sink = p; return p; }
NOINLINE static void* pong_site() { void* p = ::operator new(64); g_sink = p; return p; }
NOINLINE static void* pong_spare_site() { void* p = ::operator new(64); g_sink = p; return p; }
NOINLINE static void ping_pong_leak_site() {
    for (auto& p : g_leaked) { p = std::malloc(96); g_sink = p; }
}
NOINLINE static void* realloc_initial_site() { void* p = std::malloc(kMiB); g_sink = p; return p; }
NOINLINE static void* realloc_stress_site(void* p, size_t n) { return std::realloc(p, n); }
NOINLINE static void contender_site() {
    for (int i = 0; i < kResizes; ++i) {
        void* m = std::malloc(kMiB);
        g_sink = m;
        std::free(m);
    }
}

static void thread_a() {
    for (int r = 0; r < kRounds; ++r) {
        g_to_b.store(ping_site());
        ::operator delete(receive(g_to_a));
    }
}

static void thread_b() {
    void* spare = pong_spare_site();
    for (int r = 0; r < kRounds; ++r) {
        ::operator delete(receive(g_to_b));
        g_to_a.store(pong_site());
        if (r == kLeaks) ping_pong_leak_site();
    }
    ::operator delete(spare);
}

int main(int argc, char** argv) {
    {
        std::thread a(thread_a), b(thread_b);
        a.join();
        b.join();
    }
    {
        std::thread resizer([] {
            void* p = realloc_initial_site();
            for (int i = 0; i < kResizes; ++i) {
                p = realloc_stress_site(p, (i % 2 == 0 ? 1 : 3) * kMiB);
                g_sink = p;
            }
            std::free(p);
        });
        std::thread contenders[kContenders];
        for (auto& t : contenders) t = std::thread(contender_site);
        resizer.join();
        for (auto& t : contenders) t.join();
    }

    hs_expected::Expected e("ping-pong");
    auto& a = e.site("ping_site", "operator new");
    a.allocs = kRounds; a.alloc_bytes = 64ull * kRounds; a.frees = a.allocs; a.freed_bytes = a.alloc_bytes;
    auto& b = e.site("pong_site", "operator new");
    b.allocs = kRounds; b.alloc_bytes = 64ull * kRounds; b.frees = b.allocs; b.freed_bytes = b.alloc_bytes;
    auto& sp = e.site("pong_spare_site", "operator new");
    sp.allocs = 1; sp.alloc_bytes = 64; sp.frees = 1; sp.freed_bytes = 64;
    auto& l = e.site("ping_pong_leak_site", "malloc");
    l.allocs = kLeaks; l.alloc_bytes = 96ull * kLeaks;
    auto& ri = e.site("realloc_initial_site", "malloc");
    ri.allocs = 1; ri.alloc_bytes = kMiB; ri.frees = 1; ri.freed_bytes = kMiB;
    auto& r = e.site("realloc_stress_site", "realloc");
    r.allocs = kResizes; r.alloc_bytes = (uint64_t)(kResizes / 2) * (1 + 3) * kMiB;
    r.frees = r.allocs; r.freed_bytes = r.alloc_bytes;
    auto& c = e.site("contender_site", "malloc");
    c.allocs = (uint64_t)kContenders * kResizes; c.alloc_bytes = c.allocs * kMiB;
    c.frees = c.allocs; c.freed_bytes = c.alloc_bytes;
    e.write(argc, argv);

    std::printf("ping-pong: %d hand-offs each way, %d resizes against %d contenders\n", kRounds, kResizes, kContenders);
    return 0;
}
