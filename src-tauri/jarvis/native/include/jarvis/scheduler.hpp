#pragma once

#include "jarvis/runtime.hpp"

#include <atomic>
#include <condition_variable>
#include <cstdint>
#include <functional>
#include <memory>
#include <mutex>
#include <optional>
#include <stop_token>
#include <thread>

namespace jarvis {

enum class Priority : std::uint8_t { background = 0, normal = 1, interactive = 2, control = 3 };

struct ScheduledRequest {
  InferenceRequest request{};
  Priority priority{Priority::normal};
};

class LatestOnlyScheduler {
 public:
  using Completion = std::function<void(InferenceResult)>;

  LatestOnlyScheduler(IOmniRuntime& runtime, Completion completion);
  ~LatestOnlyScheduler();
  LatestOnlyScheduler(const LatestOnlyScheduler&) = delete;
  LatestOnlyScheduler& operator=(const LatestOnlyScheduler&) = delete;

  void start();
  void stop() noexcept;
  void submit(ScheduledRequest request);
  void cancel(std::uint64_t request_id) noexcept;
  [[nodiscard]] bool busy() const noexcept;

 private:
  void run(std::stop_token stop);

  IOmniRuntime& runtime_;
  Completion completion_;
  mutable std::mutex mutex_{};
  std::condition_variable_any ready_{};
  std::optional<ScheduledRequest> pending_{};
  std::uint64_t next_generation_{};
  std::uint64_t current_generation_{};
  std::uint64_t active_id_{};
  Priority active_priority_{Priority::background};
  std::shared_ptr<std::atomic_bool> active_cancel_{};
  std::jthread thread_{};
};

} // namespace jarvis
