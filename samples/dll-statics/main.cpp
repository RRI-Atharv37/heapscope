// dll-statics: the heap block lives in dll_statics_lib.dll and is freed by that DLL's static
// destructor during process shutdown. A correct trace has no live block that came from the DLL.
// usage: dll-statics [--expected file]   (the option is accepted and ignored)
#include <cstdio>

extern "C" __declspec(dllimport) void ds_fill();

int main() {
    ds_fill();
    std::printf("dll-statics: the DLL holds one 7777-byte block until it is unloaded\n");
    return 0;
}
