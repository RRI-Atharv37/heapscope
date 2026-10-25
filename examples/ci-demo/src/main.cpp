// ci-demo: a small "startup" scenario for the HeapScope CI gate.
//
// The clean build allocates a working set, uses it and frees all of it. Built with
// -DDEMO_REGRESSION=ON it also keeps a bigger cache alive for the whole run and leaks a buffer on
// every request, the kind of change a code review does not catch and the gate does.
#include <cstdio>
#include <cstdlib>
#include <vector>

#if defined(_MSC_VER)
#define NOINLINE __declspec(noinline)
#else
#define NOINLINE __attribute__((noinline))
#endif

static void* volatile g_sink;

#ifdef DEMO_REGRESSION
constexpr unsigned kCacheEntries = 600;   // was 200: the peak triples
#else
constexpr unsigned kCacheEntries = 200;
#endif
constexpr unsigned kEntryBytes = 4096;
constexpr unsigned kRequests = 50;

NOINLINE static void build_cache(std::vector<void*>& cache) {
    for (unsigned i = 0; i < kCacheEntries; ++i) {
        void* p = std::malloc(kEntryBytes);
        g_sink = p;
        cache.push_back(p);
    }
}

NOINLINE static void handle_request() {
    void* scratch = ::operator new(1024);
    g_sink = scratch;
#ifdef DEMO_REGRESSION
    // The regression: the scratch buffer is never released.
    (void)scratch;
#else
    ::operator delete(scratch);
#endif
}

int main() {
    std::vector<void*> cache;
    cache.reserve(kCacheEntries);
    build_cache(cache);
    for (unsigned i = 0; i < kRequests; ++i) handle_request();
    for (void* p : cache) std::free(p);
    std::printf("ci-demo: %u cache entries, %u requests\n", kCacheEntries, kRequests);
    return 0;
}
