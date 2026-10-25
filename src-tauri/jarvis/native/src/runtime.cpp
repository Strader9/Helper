#include "jarvis/runtime.hpp"

#include <stdexcept>
#include <utility>

namespace jarvis {
namespace {
class StubOmniRuntime final : public IOmniRuntime {
 public:
  void load(std::string model_path) override {
    if (model_path.empty()) throw std::invalid_argument("model path is empty");
    model_path_ = std::move(model_path); ready_ = true;
  }
  void unload() noexcept override { ready_ = false; model_path_.clear(); }
  bool ready() const noexcept override { return ready_; }
  InferenceResult infer(const InferenceRequest& request, const std::atomic_bool& cancel) override {
    if (!ready_) throw std::runtime_error("OmniRuntime is not loaded");
    if (cancel.load()) return {request.id, {}, true};
    return {request.id, "[stub OmniRuntime] model boundary is ready; inference backend not linked", false};
  }
 private:
  std::string model_path_{}; bool ready_{};
};
} // namespace
std::unique_ptr<IOmniRuntime> make_stub_omni_runtime() { return std::make_unique<StubOmniRuntime>(); }
} // namespace jarvis
