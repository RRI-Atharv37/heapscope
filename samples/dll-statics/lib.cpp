// dll-statics (library half): a DLL whose global container is filled at run time and freed by its
// static destructor, which runs at DLL_PROCESS_DETACH, i.e. inside process shutdown.
//   kStaticBytes: one block of this size, allocated by ds_fill() from `statics_site`, freed at detach.
// If HEAPSCOPE_DS_MARKER names a file, the destructor creates it, so a test can tell that the
// destructor really ran (otherwise "no leak" would prove nothing).
#include <cstdlib>
#include <windows.h>

#if defined(_MSC_VER)
#define NOINLINE __declspec(noinline)
#else
#define NOINLINE __attribute__((noinline))
#endif

constexpr unsigned kStaticBytes = 7777;

namespace {
struct Holder {
    char* block = nullptr;
    ~Holder() {
        std::free(block);
        char path[MAX_PATH];
        DWORD n = GetEnvironmentVariableA("HEAPSCOPE_DS_MARKER", path, sizeof path);
        if (n > 0 && n < sizeof path) {
            HANDLE h = CreateFileA(path, GENERIC_WRITE, 0, nullptr, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, nullptr);
            if (h != INVALID_HANDLE_VALUE) CloseHandle(h);
        }
    }
};
Holder g_holder;
}  // namespace

NOINLINE static void statics_site() { g_holder.block = static_cast<char*>(std::malloc(kStaticBytes)); }

extern "C" __declspec(dllexport) void ds_fill() { statics_site(); }
