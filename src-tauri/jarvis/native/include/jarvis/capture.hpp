#pragma once

#include "jarvis/audio.hpp"

#include <cstdint>
#include <optional>
#include <vector>

namespace jarvis {

struct VideoFrame {
  std::uint32_t width{};
  std::uint32_t height{};
  std::uint32_t row_pitch{};
  std::uint64_t timestamp_100ns{};
  std::vector<std::byte> bgra{};
};

class IDesktopCapture {
 public:
  virtual ~IDesktopCapture() = default;
  virtual void start() = 0;
  virtual void stop() noexcept = 0;
  [[nodiscard]] virtual std::optional<VideoFrame> next_frame(std::uint32_t timeout_ms) = 0;
};

class IAudioCapture {
 public:
  virtual ~IAudioCapture() = default;
  virtual void start() = 0;
  virtual void stop() noexcept = 0;
  [[nodiscard]] virtual std::optional<audio::PcmBlock> next_block(std::uint32_t timeout_ms) = 0;
};

} // namespace jarvis
