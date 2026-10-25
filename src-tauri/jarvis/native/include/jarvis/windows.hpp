#pragma once

#ifdef _WIN32

#include "jarvis/capture.hpp"
#include "jarvis/worker.hpp"

#include <atomic>
#include <memory>
#include <string>

namespace jarvis::win {

class NamedPipeServer {
 public:
  NamedPipeServer(std::wstring pipe_name, Worker& worker);
  ~NamedPipeServer();
  NamedPipeServer(const NamedPipeServer&) = delete;
  NamedPipeServer& operator=(const NamedPipeServer&) = delete;

  int run();
  void request_stop() noexcept;

 private:
  struct Impl;
  std::unique_ptr<Impl> impl_;
};

[[nodiscard]] std::unique_ptr<IDesktopCapture> make_dxgi_desktop_capture();
[[nodiscard]] std::unique_ptr<IAudioCapture> make_wasapi_loopback_capture();

} // namespace jarvis::win

#endif
