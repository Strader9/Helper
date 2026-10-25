#ifdef _WIN32
#include "jarvis/windows.hpp"

#include <Windows.h>

#include <iostream>
#include <stdexcept>
#include <string>
#include <string_view>
#include <utility>

namespace {
std::string to_utf8(std::wstring_view value) {
  if (value.empty()) return {};
  const auto size = WideCharToMultiByte(
      CP_UTF8, WC_ERR_INVALID_CHARS, value.data(), static_cast<int>(value.size()),
      nullptr, 0, nullptr, nullptr);
  if (size <= 0) throw std::runtime_error("model path is not valid UTF-16");
  std::string result(static_cast<std::size_t>(size), '\0');
  if (WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, value.data(),
                          static_cast<int>(value.size()), result.data(), size,
                          nullptr, nullptr) <= 0) {
    throw std::runtime_error("unable to encode model path as UTF-8");
  }
  return result;
}
}  // namespace

int wmain(int argc, wchar_t** argv) {
  const std::wstring pipe = argc > 1 ? argv[1] : LR"(\\.\pipe\AIJarvis.Worker.v1)";
  const std::string model = argc > 2 ? to_utf8(argv[2]) : "";
  if (model.empty()) { std::cerr << "model path is required\n"; return 2; }
#ifdef JARVIS_ENABLE_STUB_RUNTIME
  auto runtime = jarvis::make_stub_omni_runtime();
#elif defined(JARVIS_HAS_REAL_RUNTIME)
  auto runtime = jarvis::make_real_omni_runtime();
#else
#error "A production worker must link the real runtime provider"
#endif
  jarvis::Worker worker(std::move(runtime));
  if (!worker.start(model)) { std::cerr << "failed to start worker runtime\n"; return 1; }
  jarvis::win::NamedPipeServer server(pipe, worker);
  return server.run();
}
#endif
