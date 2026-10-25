#include "jarvis/scheduler.hpp"

#include <exception>
#include <string>
#include <utility>

namespace jarvis {
LatestOnlyScheduler::LatestOnlyScheduler(IOmniRuntime& runtime, Completion completion)
    : runtime_(runtime), completion_(std::move(completion)) {}
LatestOnlyScheduler::~LatestOnlyScheduler() { stop(); }
void LatestOnlyScheduler::start() {
  std::lock_guard lock(mutex_);
  if (!thread_.joinable()) thread_ = std::jthread([this](std::stop_token stop) { run(stop); });
}
void LatestOnlyScheduler::stop() noexcept {
  if (thread_.joinable()) {
    {
      std::lock_guard lock(mutex_);
      pending_.reset();
      if (active_cancel_) active_cancel_->store(true);
    }
    thread_.request_stop(); ready_.notify_all(); thread_.join();
  }
}
void LatestOnlyScheduler::submit(ScheduledRequest request) {
  std::lock_guard lock(mutex_);
  const bool accepted = !pending_ || request.priority >= pending_->priority;
  if (accepted) {
    pending_ = std::move(request);
    ++next_generation_;
    current_generation_ = next_generation_;
    if (active_cancel_ && pending_->priority >= Priority::interactive &&
        pending_->priority >= active_priority_) {
      active_cancel_->store(true);
    }
  }
  ready_.notify_one();
}
void LatestOnlyScheduler::cancel(std::uint64_t id) noexcept {
  std::lock_guard lock(mutex_);
  if (pending_ && pending_->request.id == id) pending_.reset();
  if (active_id_ == id && active_cancel_) active_cancel_->store(true);
}
bool LatestOnlyScheduler::busy() const noexcept {
  std::lock_guard lock(mutex_); return active_id_ != 0 || pending_.has_value();
}
void LatestOnlyScheduler::run(std::stop_token stop) {
  while (!stop.stop_requested()) {
    ScheduledRequest work; std::shared_ptr<std::atomic_bool> cancel; std::uint64_t generation{};
    {
      std::unique_lock lock(mutex_);
      ready_.wait(lock, stop, [this] { return pending_.has_value(); });
      if (stop.stop_requested()) break;
      work = std::move(*pending_); pending_.reset();
      generation = current_generation_;
      active_id_ = work.request.id; active_priority_ = work.priority;
      active_cancel_ = cancel = std::make_shared<std::atomic_bool>(false);
    }
    InferenceResult result;
    try {
      result = runtime_.infer(work.request, *cancel);
    } catch (const std::exception& error) {
      result = {work.request.id, std::string("runtime error: ") + error.what(), false};
    } catch (...) {
      result = {work.request.id, "runtime error: unknown exception", false};
    }
    result.cancelled = result.cancelled || cancel->load();
    bool stale{};
    {
      std::lock_guard lock(mutex_);
      stale = generation != current_generation_ && work.priority <= Priority::normal;
      active_id_ = 0; active_priority_ = Priority::background; active_cancel_.reset();
    }
    if (completion_ && !stale) completion_(std::move(result));
  }
}
} // namespace jarvis
