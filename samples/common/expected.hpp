// Ground-truth helper: each sample describes what it deliberately did, so tests can compare
// HeapScope's output with the truth. Hand-rolled JSON on purpose: no dependencies, and nothing
// is allocated while writing (the file is written after the measured work is finished).
#pragma once
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>

namespace hs_expected {

struct Site {
    const char* function = "";  // name of the (noinline) function that performs the allocations
    const char* api = "";       // "operator new" | "malloc" | "pool"
    uint64_t allocs = 0;
    uint64_t alloc_bytes = 0;
    uint64_t frees = 0;
    uint64_t freed_bytes = 0;
};

class Expected {
public:
    explicit Expected(const char* sample) : sample_(sample) {}

    Site& site(const char* function, const char* api) {
        sites_[count_].function = function;
        sites_[count_].api = api;
        return sites_[count_++];
    }

    // Path: "--expected <file>" argument, else env HEAPSCOPE_EXPECTED, else "expected.json".
    void write(int argc, char** argv) const {
        const char* path = std::getenv("HEAPSCOPE_EXPECTED");
        for (int i = 1; i + 1 < argc; ++i)
            if (std::strcmp(argv[i], "--expected") == 0) path = argv[i + 1];
        if (!path) path = "expected.json";
        std::FILE* f = std::fopen(path, "w");
        if (!f) { std::perror("expected.json"); return; }
        std::fprintf(f, "{\n  \"schema\": 0,\n  \"sample\": \"%s\",\n  \"sites\": [\n", sample_);
        for (int i = 0; i < count_; ++i) {
            const Site& s = sites_[i];
            std::fprintf(f,
                "    {\"function\": \"%s\", \"api\": \"%s\", \"allocs\": %llu, \"alloc_bytes\": %llu, "
                "\"frees\": %llu, \"freed_bytes\": %llu, \"live_allocs_at_exit\": %llu, \"live_bytes_at_exit\": %llu}%s\n",
                s.function, s.api, (unsigned long long)s.allocs, (unsigned long long)s.alloc_bytes,
                (unsigned long long)s.frees, (unsigned long long)s.freed_bytes,
                (unsigned long long)(s.allocs - s.frees), (unsigned long long)(s.alloc_bytes - s.freed_bytes),
                i + 1 < count_ ? "," : "");
        }
        std::fprintf(f, "  ]\n}\n");
        std::fclose(f);
    }

private:
    const char* sample_;
    Site sites_[16];
    int count_ = 0;
};

}  // namespace hs_expected
