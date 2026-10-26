cmake_minimum_required(VERSION 3.20)
project(heapscope_samples C CXX)

set(CMAKE_C_STANDARD 11)
set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)

if(NOT MSVC)
  message(FATAL_ERROR "HeapScope samples target MSVC on Windows.")
endif()

# PDBs in every configuration (Debug and RelWithDebInfo are the supported ones). No incremental
# linking, so the exe/PDB pair stays stable for symbol resolution.
add_compile_options(/W4 /Zi)
add_compile_definitions(_CRT_SECURE_NO_WARNINGS)
add_link_options(/DEBUG /INCREMENTAL:NO)

# Dynamic CRT by default. HeapScope hooks the ntdll heap layer, so a static CRT (/MT) is also covered.
set(CMAKE_MSVC_RUNTIME_LIBRARY "MultiThreaded$<$<CONFIG:Debug>:Debug>DLL")

foreach(sample basic-alloc leak growth custom-pool churn multithread thread-churn heap-exception private-heap ping-pong)
  add_executable(${sample} ${sample}/main.cpp)
endforeach()

# A DLL whose static destructor frees a block during process shutdown.
add_library(dll-statics-lib SHARED dll-statics/lib.cpp)
set_target_properties(dll-statics-lib PROPERTIES OUTPUT_NAME dll_statics_lib)
add_executable(dll-statics dll-statics/main.cpp)
target_link_libraries(dll-statics PRIVATE dll-statics-lib)

# Plain C: proves include/heapscope/heapscope.h compiles as C and works from several threads. Built
# as C11 and again as C89/C90 (MSVC's default C mode: no mixed declarations, no C99 library).
add_executable(header-c header-c/main.c)
add_executable(header-c89 header-c/main.c)
set_target_properties(header-c89 PROPERTIES C_STANDARD 90 C_STANDARD_REQUIRED ON)

# C++: the header must leave the code around it alone (no <windows.h>, no WIN32_LEAN_AND_MEAN, no
# min/max macros) and work in two translation units, in either include order.
add_executable(header-cxx header-cxx/main.cpp header-cxx/other.cpp)
