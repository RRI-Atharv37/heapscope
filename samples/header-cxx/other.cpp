// Second translation unit of header-cxx: <windows.h> first, then heapscope.h. Each unit keeps its
// own lookup state, and both must work.
#include <windows.h>

#include "../../include/heapscope/heapscope.h"

static char g_slot[24];
static int g_pool;

int other_tu_marks_and_allocs() {
    HEAPSCOPE_MARK("cxx-other");
    HEAPSCOPE_ALLOC(g_slot, sizeof g_slot, &g_pool);
    HEAPSCOPE_FREE(g_slot, &g_pool);
    Sleep(1);
    return 7;
}
