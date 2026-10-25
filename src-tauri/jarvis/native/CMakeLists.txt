cmake_minimum_required(VERSION 3.24)

option(JARVIS_ENABLE_STUB_RUNTIME "Build the non-inference runtime used by tests and explicit development mode" OFF)

add_library(jarvis_worker_core STATIC
  src/protocol.cpp
  src/audio.cpp
  src/fingerprint.cpp
  src/scheduler.cpp
  src/runtime.cpp
  src/worker.cpp
)
target_link_libraries(jarvis_worker_core PUBLIC jarvis::runtime_boundary)
target_include_directories(jarvis_worker_core
  PUBLIC include
  PRIVATE "${PROJECT_SOURCE_DIR}/third_party/runtime/vendor/vendor"
)
target_compile_features(jarvis_worker_core PUBLIC cxx_std_20)
target_compile_definitions(jarvis_worker_core PRIVATE $<$<BOOL:${WIN32}>:NOMINMAX;WIN32_LEAN_AND_MEAN>)
if(MSVC)
  # The pinned omni headers are UTF-8 without a BOM. The default Chinese
  # Windows code page can otherwise reinterpret multibyte bytes as syntax.
  target_compile_options(jarvis_worker_core PRIVATE /utf-8)
endif()

if(TARGET jarvis::runtime_provider AND NOT JARVIS_ENABLE_STUB_RUNTIME)
  target_sources(jarvis_worker_core PRIVATE src/omni_runtime.cpp)
  target_link_libraries(jarvis_worker_core PRIVATE jarvis::runtime_provider)
endif()

if(WIN32)
  target_link_libraries(jarvis_worker_core PRIVATE user32)
  add_library(jarvis_worker_windows STATIC
    src/windows/named_pipe_server.cpp
    src/windows/dxgi_capture.cpp
    src/windows/wasapi_capture.cpp
  )
  target_include_directories(jarvis_worker_windows PUBLIC include)
  target_compile_features(jarvis_worker_windows PUBLIC cxx_std_20)
  target_compile_definitions(jarvis_worker_windows PRIVATE NOMINMAX WIN32_LEAN_AND_MEAN)
  target_link_libraries(jarvis_worker_windows PUBLIC jarvis_worker_core d3d11 dxgi ole32 avrt uuid)

  add_executable(jarvis-native-worker src/main.cpp)
  target_link_libraries(jarvis-native-worker PRIVATE jarvis_worker_windows)
  if(JARVIS_ENABLE_STUB_RUNTIME)
    target_compile_definitions(jarvis-native-worker PRIVATE JARVIS_ENABLE_STUB_RUNTIME=1)
  elseif(TARGET jarvis::runtime_provider)
    target_compile_definitions(jarvis-native-worker PRIVATE JARVIS_HAS_REAL_RUNTIME=1)
  else()
    message(FATAL_ERROR
      "Production worker requires jarvis::runtime_provider; configure JARVIS_RUNTIME_ENABLE_UPSTREAM=ON")
  endif()
endif()

include(CTest)
if(BUILD_TESTING)
  add_executable(jarvis-native-tests tests/native_tests.cpp)
  target_link_libraries(jarvis-native-tests PRIVATE jarvis_worker_core)
  if(MSVC)
    target_compile_options(jarvis-native-tests PRIVATE /utf-8)
  endif()
  add_test(NAME jarvis-native-tests COMMAND jarvis-native-tests)
endif()
