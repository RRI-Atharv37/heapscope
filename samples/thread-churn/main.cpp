// thread-churn: many short-lived threads, one after the other. Every block the program makes is
// freed, but each thread start-up and exit also makes and frees blocks the program never asked for
// (loader, FLS and CRT per-thread data), which the capture must keep balanced.
//   churn_thread_site: N threads x 4 x 211 B, all freed before the thread ends
// usage: thread-churn [threads] [--expected file]      (default 64 threads)
#include "../common/expected.hpp"

#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <string>
#include <thread>

#if defined(_MSC_VER)
#define NOINLINE __declspec(noinline)
#else
#define NOINLINE __attribute__((noinline))
#endif

static void* volatile g_sink;

NOINLINE static void churn_thread_site() {
    void* p[4];
    for (auto& q : p) { q = std::malloc(211); g_sink = q; }
    for (auto& q : p) std::free(q);
}

int main(int argc, char** argv) {
    int threads = 64;
    for (int i = 1; i < argc; ++i) {
        if (std::strcmp(argv[i], "--expected") == 0) { ++i; continue; }
        threads = std::atoi(argv[i]);
    }
    for (int t = 0; t < threads; ++t) {
        std::thread([] {
            thread_local std::string name = std::string(100, 'x');  // per-thread CRT state, freed at thread exit
            g_sink = &name;
            churn_thread_site();
        }).join();
    }

    hs_expected::Expected e("thread-churn");
    auto& a = e.site("churn_thread_site", "malloc");
    a.allocs = 4ull * threads; a.alloc_bytes = 211ull * 4 * threads;
    a.frees = a.allocs; a.freed_bytes = a.alloc_bytes;
    e.write(argc, argv);

    std::printf("thread-churn: %d threads\n", threads);
    return 0;
}
