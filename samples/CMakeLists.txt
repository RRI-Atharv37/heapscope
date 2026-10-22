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

foreach(sample basic-alloc leak growth custom-pool churn multithread)
  add_executable(${sample} ${sample}/main.cpp)
endforeach()

# Plain C: proves include/heapscope/heapscope.h compiles as C and works from several threads.
add_executable(header-c header-c/main.c)
