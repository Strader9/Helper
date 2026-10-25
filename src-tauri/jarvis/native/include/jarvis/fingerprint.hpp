#pragma once

#include "jarvis/capture.hpp"

#include <chrono>
#include <cstdint>
#include <random>
#include <vector>

namespace jarvis {

[[nodiscard]] std::uint64_t frame_fingerprint(const VideoFrame& frame) noexcept;

class FrameDeduplicator {
 public:
  [[nodiscard]] bool changed(const VideoFrame& frame) noexcept;
  void reset() noexcept { initialized_ = false; }

 private:
  bool initialized_{};
  std::uint64_t last_{};
};

class FrameChangeDetector {
 public:
  explicit FrameChangeDetector(std::uint8_t pixel_threshold = 18,
                               float changed_ratio_threshold = 0.03F,
                               float average_delta_threshold = 3.0F)
      : pixel_threshold_(pixel_threshold),
        changed_ratio_threshold_(changed_ratio_threshold),
        average_delta_threshold_(average_delta_threshold) {}

  [[nodiscard]] bool changed(const VideoFrame& frame) noexcept;
  void reset() noexcept { previous_.clear(); }

 private:
  std::uint8_t pixel_threshold_;
  float changed_ratio_threshold_;
  float average_delta_threshold_;
  std::vector<std::uint8_t> previous_{};
};

enum class ScreenIdleEvent : std::uint8_t {
  none,
  entered_idle,
  reminder_due,
  resumed,
};

class ScreenIdleMonitor {
 public:
  using Clock = std::chrono::steady_clock;
  using Duration = Clock::duration;

  explicit ScreenIdleMonitor(
      Duration idle_after = std::chrono::minutes(2),
      Duration reminder_min = std::chrono::seconds(60),
      Duration reminder_max = std::chrono::seconds(120));
  ScreenIdleMonitor(Duration idle_after, Duration reminder_min,
                    Duration reminder_max, std::uint32_t random_seed);

  [[nodiscard]] ScreenIdleEvent observe(bool changed, Clock::time_point now);
  [[nodiscard]] bool idle() const noexcept { return idle_; }
  void reset() noexcept;

 private:
  [[nodiscard]] Duration next_reminder_delay();

  Duration idle_after_;
  Duration reminder_min_;
  Duration reminder_max_;
  std::mt19937 random_;
  bool initialized_{};
  bool idle_{};
  Clock::time_point last_change_{};
  Clock::time_point next_reminder_{};
};

} // namespace jarvis
