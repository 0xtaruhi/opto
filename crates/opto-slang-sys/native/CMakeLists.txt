# SPDX-FileCopyrightText: 2026 Zhengyi Zhang
# SPDX-License-Identifier: GPL-3.0-only

cmake_minimum_required(VERSION 3.20)

project(opto_slang_bridge LANGUAGES CXX)

if(NOT DEFINED OPTO_SLANG_VENDOR_DIR)
  message(FATAL_ERROR "OPTO_SLANG_VENDOR_DIR is required")
endif()

if(NOT DEFINED OPTO_FMT_VENDOR_DIR)
  message(FATAL_ERROR "OPTO_FMT_VENDOR_DIR is required")
endif()

set(SLANG_INCLUDE_TOOLS OFF CACHE BOOL "" FORCE)
set(SLANG_INCLUDE_TESTS OFF CACHE BOOL "" FORCE)
set(SLANG_INCLUDE_DOCS OFF CACHE BOOL "" FORCE)
set(SLANG_INCLUDE_PYLIB OFF CACHE BOOL "" FORCE)
set(SLANG_INCLUDE_PYTHON_DOCS OFF CACHE BOOL "" FORCE)
set(SLANG_INCLUDE_INSTALL OFF CACHE BOOL "" FORCE)
set(SLANG_INCLUDE_COVERAGE OFF CACHE BOOL "" FORCE)
set(SLANG_INCLUDE_THREADTEST OFF CACHE BOOL "" FORCE)
set(SLANG_INCLUDE_UVM_TEST OFF CACHE BOOL "" FORCE)
set(SLANG_INCLUDE_BENCHMARKS OFF CACHE BOOL "" FORCE)
set(BUILD_SHARED_LIBS OFF CACHE BOOL "" FORCE)
set(SLANG_USE_MIMALLOC OFF CACHE BOOL "" FORCE)
set(FETCHCONTENT_SOURCE_DIR_FMT "${OPTO_FMT_VENDOR_DIR}" CACHE PATH "" FORCE)
set(CMAKE_POSITION_INDEPENDENT_CODE ON)

add_subdirectory("${OPTO_SLANG_VENDOR_DIR}" "${CMAKE_BINARY_DIR}/slang" EXCLUDE_FROM_ALL)

add_library(
  opto_slang_bridge STATIC
  opto_slang_bridge.cpp
  opto_slang_lower_support.cpp
  opto_slang_lower_expr.cpp
  opto_slang_lower_process.cpp
  opto_slang_lower_hierarchy.cpp
  opto_slang_views.cpp)
target_compile_features(opto_slang_bridge PRIVATE cxx_std_20)
target_include_directories(opto_slang_bridge PUBLIC "${CMAKE_CURRENT_SOURCE_DIR}")
target_link_libraries(opto_slang_bridge PRIVATE slang::slang)
set_target_properties(
  opto_slang_bridge
  PROPERTIES
    CXX_VISIBILITY_PRESET hidden
    POSITION_INDEPENDENT_CODE ON
    VISIBILITY_INLINES_HIDDEN YES)
