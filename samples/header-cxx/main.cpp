// header-cxx: heapscope.h must not change the code around it. This file includes the header first
// and then uses what <windows.h> would break if the header had pulled it in (the min/max macros)
// and what WIN32_LEAN_AND_MEAN would hide (shellapi.h). `other.cpp` includes <windows.h> first, so
// both orders are covered. The macros must also evaluate their arguments at most once.
// Exit code 0 when all of that holds.
#include "../../include/heapscope/heapscope.h"

#ifdef WIN32_LEAN_AND_MEAN
#error heapscope.h must not define WIN32_LEAN_AND_MEAN
#endif
#ifdef _WINDOWS_
#error heapscope.h must not include <windows.h>
#endif
#ifdef min
#error heapscope.h must not define min
#endif

#include <algorithm>
#include <cstdio>
#include <limits>

#include <windows.h>
#include <shellapi.h>  // not part of WIN32_LEAN_AND_MEAN

#undef min
#undef max

int other_tu_marks_and_allocs();

static int g_pool_evaluations = 0;
static int g_pool_object = 0;
static const void* next_pool() { ++g_pool_evaluations; return &g_pool_object; }

int main() {
    int largest = std::numeric_limits<int>::max();
    int smallest = std::min(3, 4);
    HEAPSCOPE_MARK("cxx-main");

    char block[16];
    char grown[32];
    HEAPSCOPE_ALLOC(block, sizeof block, next_pool());
    HEAPSCOPE_REALLOC(block, grown, sizeof grown, next_pool());
    HEAPSCOPE_FREE(grown, next_pool());
    // At most one evaluation per macro. ALLOC and FREE look at their arguments only when the capture
    // DLL is there; REALLOC always evaluates its pool once.
    bool pool_ok = g_pool_evaluations == 1 || g_pool_evaluations == 3;

    int other = other_tu_marks_and_allocs();
    bool shell_ok = &ShellExecuteA != nullptr;  // shellapi.h is available
    std::printf("header-cxx: max %d min %d pool evaluations %d other %d\n", largest, smallest,
                g_pool_evaluations, other);
    return largest == 2147483647 && smallest == 3 && pool_ok && shell_ok && other == 7 ? 0 : 1;
}
