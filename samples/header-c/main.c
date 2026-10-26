/* header-c: heapscope.h used from plain C. Checks that the header compiles as C, resolves safely
 * from several threads at once, and that all four macros reach the trace.
 *   4 threads x 100 blocks of 32 B   (first macro use races on every thread; never freed)
 *   10 blocks of 48 B, the first 4 freed, the 5th resized to 96 B
 *   markers "c-start" and "c-end"
 * Live at exit: 400 x 32 B + 5 x 48 B + 1 x 96 B.
 * Built twice: as C11 (header-c) and as C89/C90 (header-c89). */
#include "../../include/heapscope/heapscope.h"

/* After heapscope.h on purpose: the header must not need, or clash with, <windows.h>. */
#include <windows.h>
#include <stdio.h>

#define THREADS 4
#define PER_THREAD 100

static unsigned char g_slab[THREADS][PER_THREAD][32];
static unsigned char g_blocks[10][48];
static unsigned char g_big[96];
static int g_pool;

static DWORD WINAPI thread_main(LPVOID arg) {
    int t = (int)(size_t)arg;
    int i;
    for (i = 0; i < PER_THREAD; ++i) HEAPSCOPE_ALLOC(g_slab[t][i], sizeof g_slab[t][i], &g_pool);
    return 0;
}

#if defined(_MSC_VER)
#define NOINLINE __declspec(noinline)
#else
#define NOINLINE __attribute__((noinline))
#endif

NOINLINE static void pool_user_c(void) {
    int i;
    for (i = 0; i < 10; ++i) HEAPSCOPE_ALLOC(g_blocks[i], sizeof g_blocks[i], &g_pool);
    for (i = 0; i < 4; ++i) HEAPSCOPE_FREE(g_blocks[i], &g_pool);
    HEAPSCOPE_REALLOC(g_blocks[4], g_big, sizeof g_big, &g_pool);
}

int main(void) {
    HANDLE th[THREADS];
    int t;
    for (t = 0; t < THREADS; ++t) th[t] = CreateThread(NULL, 0, thread_main, (LPVOID)(size_t)t, 0, NULL);
    WaitForMultipleObjects(THREADS, th, TRUE, INFINITE);
    for (t = 0; t < THREADS; ++t) CloseHandle(th[t]);

    HEAPSCOPE_MARK("c-start");
    pool_user_c();
    HEAPSCOPE_MARK("c-end");
    printf("header-c: 400 + 10 pool allocations, 4 freed, 1 resized\n");
    return 0;
}
