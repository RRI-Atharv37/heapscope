cmake_minimum_required(VERSION 3.20)
project(heapscope_ci_demo CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)

if(NOT MSVC)
  message(FATAL_ERROR "HeapScope targets MSVC on Windows.")
endif()

# Seeds the regression: the same program, with a cache that grows and a buffer that is never freed.
option(DEMO_REGRESSION "Build the version with a memory regression" OFF)

# PDBs in every configuration, so HeapScope can name the functions and lines.
add_compile_options(/W4 /Zi)
add_link_options(/DEBUG /INCREMENTAL:NO)

add_executable(ci-demo src/main.cpp)
if(DEMO_REGRESSION)
  target_compile_definitions(ci-demo PRIVATE DEMO_REGRESSION=1)
endif()
