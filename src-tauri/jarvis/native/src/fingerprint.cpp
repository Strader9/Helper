#include "jarvis/fingerprint.hpp"

#include <algorithm>
#include <cmath>
#include <random>
#include <vector>

namespace jarvis {
namespace {
std::vector<std::uint8_t> sample_luminance(const VideoFrame& frame) {
  std::vector<std::uint8_t> samples;
  samples.reserve(32U * 18U);
  if (!frame.width || !frame.height || frame.bgra.empty()) return samples;
  for (std::uint32_t gy = 0; gy < 18; ++gy) {
    const auto y = std::min((gy * frame.height) / 18U, frame.height - 1);
    for (std::uint32_t gx = 0; gx < 32; ++gx) {
      const auto x = std::min((gx * frame.width) / 32U, frame.width - 1);
      const auto at = std::size_t(y) * frame.row_pitch + std::size_t(x) * 4U;
      if (at + 2 >= frame.bgra.size()) {
        samples.push_back(0);
        continue;
      }
      const auto b = std::to_integer<unsigned>(frame.bgra[at]);
      const auto g = std::to_integer<unsigned>(frame.bgra[at + 1]);
      const auto r = std::to_integer<unsigned>(frame.bgra[at + 2]);
      samples.push_back(std::uint8_t((r * 77U + g * 150U + b * 29U) >> 8U));
    }
  }
  return samples;
}
} // namespace

std::uint64_t frame_fingerprint(const VideoFrame& frame) noexcept {
  // FNV-1a over a fixed 32x18 luminance grid: stable across row padding and inexpensive.
  std::uint64_t hash = 1469598103934665603ULL;
  if (!frame.width || !frame.height || frame.bgra.empty()) return hash;
  for (std::uint32_t gy = 0; gy < 18; ++gy) {
    const auto y = std::min((gy * frame.height) / 18U, frame.height - 1);
    for (std::uint32_t gx = 0; gx < 32; ++gx) {
      const auto x = std::min((gx * frame.width) / 32U, frame.width - 1);
      const auto at = std::size_t(y) * frame.row_pitch + std::size_t(x) * 4U;
      if (at + 2 >= frame.bgra.size()) continue;
      const auto b = std::to_integer<unsigned>(frame.bgra[at]);
      const auto g = std::to_integer<unsigned>(frame.bgra[at + 1]);
      const auto r = std::to_integer<unsigned>(frame.bgra[at + 2]);
      hash ^= std::uint8_t((r * 77U + g * 150U + b * 29U) >> 8U); hash *= 1099511628211ULL;
    }
  }
  hash ^= frame.width; hash *= 1099511628211ULL; hash ^= frame.height;
  return hash;
}
bool FrameDeduplicator::changed(const VideoFrame& frame) noexcept {
  const auto value = frame_fingerprint(frame);
  const bool result = !initialized_ || value != last_; initialized_ = true; last_ = value; return result;
}

bool FrameChangeDetector::changed(const VideoFrame& frame) noexcept {
  auto current = sample_luminance(frame);
  if (current.empty()) return false;
  if (previous_.size() != current.size()) {
    previous_ = std::move(current);
    return true;
  }

  std::size_t changed_samples{};
  std::uint64_t total_delta{};
  for (std::size_t index = 0; index < current.size(); ++index) {
    const auto delta = static_cast<unsigned>(
        std::abs(int(current[index]) - int(previous_[index])));
    total_delta += delta;
    if (delta >= pixel_threshold_) ++changed_samples;
  }
  const auto changed_ratio =
      static_cast<float>(changed_samples) / static_cast<float>(current.size());
  const auto average_delta =
      static_cast<float>(total_delta) / static_cast<float>(current.size());
  const bool result = changed_ratio >= changed_ratio_threshold_ ||
                      average_delta >= average_delta_threshold_;
  if (result) previous_ = std::move(current);
  return result;
}

ScreenIdleMonitor::ScreenIdleMonitor(Duration idle_after, Duration reminder_min,
                                     Duration reminder_max)
    : ScreenIdleMonitor(idle_after, reminder_min, reminder_max,
                        std::random_device{}()) {}

ScreenIdleMonitor::ScreenIdleMonitor(Duration idle_after, Duration reminder_min,
                                     Duration reminder_max,
                                     std::uint32_t random_seed)
    : idle_after_(std::max(idle_after, Duration::zero())),
      reminder_min_(std::max(
          reminder_min,
          std::chrono::duration_cast<Duration>(std::chrono::seconds(1)))),
      reminder_max_(std::max(reminder_max, reminder_min_)),
      random_(random_seed) {}

ScreenIdleEvent ScreenIdleMonitor::observe(bool changed, Clock::time_point now) {
  if (!initialized_) {
    initialized_ = true;
    last_change_ = now;
    return ScreenIdleEvent::none;
  }
  if (changed) {
    last_change_ = now;
    next_reminder_ = {};
    if (idle_) {
      idle_ = false;
      return ScreenIdleEvent::resumed;
    }
    return ScreenIdleEvent::none;
  }
  if (!idle_ && now - last_change_ >= idle_after_) {
    idle_ = true;
    next_reminder_ = now + next_reminder_delay();
    return ScreenIdleEvent::entered_idle;
  }
  if (idle_ && now >= next_reminder_) {
    next_reminder_ = now + next_reminder_delay();
    return ScreenIdleEvent::reminder_due;
  }
  return ScreenIdleEvent::none;
}

void ScreenIdleMonitor::reset() noexcept {
  initialized_ = false;
  idle_ = false;
  last_change_ = {};
  next_reminder_ = {};
}

ScreenIdleMonitor::Duration ScreenIdleMonitor::next_reminder_delay() {
  const auto minimum =
      std::chrono::duration_cast<std::chrono::seconds>(reminder_min_).count();
  const auto maximum =
      std::chrono::duration_cast<std::chrono::seconds>(reminder_max_).count();
  std::uniform_int_distribution<long long> seconds(minimum, maximum);
  return std::chrono::seconds(seconds(random_));
}
} // namespace jarvis
