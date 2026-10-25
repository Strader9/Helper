cmake_minimum_required(VERSION 3.24)
project(AIJarvis LANGUAGES CXX)

include(CTest)

option(JARVIS_BUILD_NATIVE_WORKER "Build the native worker" ON)
add_subdirectory(third_party/runtime)
if(JARVIS_BUILD_NATIVE_WORKER)
  add_subdirectory(native)
endif()
