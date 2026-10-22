// A deliberately simple fixed-size-slot pool carved from ONE big block. To a normal heap
// profiler this is a single allocation; the two HEAPSCOPE_* lines are the whole opt-in.
#pragma once
#include "../../include/heapscope/heapscope.h"

#include <cstddef>
#include <cstdlib>

class FixedPool {
public:
    static constexpr size_t kSlot = 64;

    explicit FixedPool(size_t slots) : slots_(slots) {
        block_ = static_cast<unsigned char*>(std::malloc(slots * kSlot));  // the one big block
    }
    ~FixedPool() { std::free(block_); }

    void* allocate() {
        void* p;
        if (free_list_) { p = free_list_; free_list_ = *static_cast<void**>(p); }
        else if (next_ < slots_) { p = block_ + (next_++) * kSlot; }
        else return nullptr;
        HEAPSCOPE_ALLOC(p, kSlot, this);  // <-- opt-in line 1
        return p;
    }

    void release(void* p) {
        HEAPSCOPE_FREE(p, this);  // <-- opt-in line 2
        *static_cast<void**>(p) = free_list_;
        free_list_ = p;
    }

private:
    unsigned char* block_;
    size_t slots_;
    size_t next_ = 0;
    void* free_list_ = nullptr;
};
