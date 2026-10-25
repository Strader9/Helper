#include "jarvis/audio.hpp"
#include "jarvis/fingerprint.hpp"
#include "jarvis/protocol.hpp"
#include "jarvis/runtime.hpp"
#include "jarvis/scheduler.hpp"
#include "jarvis/worker.hpp"

#include <algorithm>
#include <array>
#include <atomic>
#include <chrono>
#include <condition_variable>
#include <cstdlib>
#include <deque>
#include <iostream>
#include <limits>
#include <memory>
#include <mutex>
#include <span>
#include <thread>
#include <unordered_map>
#include <vector>

namespace {
void require(bool value, const char* message) { if (!value) { std::cerr << "FAIL: " << message << '\n'; std::exit(1); } }
std::span<const std::byte> bytes(const char* value, std::size_t size) {
  return {reinterpret_cast<const std::byte*>(value), size};
}
bool has_unified_perception_schema(const std::string& text) {
  return text.starts_with('{') && text.ends_with('}') &&
         std::ranges::all_of(
             std::array{std::string_view("\"scene\":"),
                        std::string_view("\"confidence\":"),
                        std::string_view("\"scene_evidence\":"),
                        std::string_view("\"observation\":"),
                        std::string_view("\"barrage_candidates\":"),
                        std::string_view("\"course_transcript\":"),
                        std::string_view("\"course_note\":"),
                        std::string_view("\"course_title\":"),
                        std::string_view("\"course_interaction\":"),
                        std::string_view("\"capture_keyframe\":"),
                        std::string_view("\"keyframe_note\":"),
                        std::string_view("\"assistant_message\":"),
                        std::string_view("\"barrage_pending\":"),
                        std::string_view("\"classification_recovered\":"),
                        std::string_view("\"barrage_source\":"),
                        std::string_view("\"barrage_fallback_reason\":")},
             [&text](std::string_view field) { return text.find(field) != std::string::npos; });
}
class BlockingRuntime final : public jarvis::IOmniRuntime {
 public:
  void load(std::string) override { ready_ = true; }
  void unload() noexcept override { ready_ = false; }
  bool ready() const noexcept override { return ready_; }
  jarvis::InferenceResult infer(const jarvis::InferenceRequest& request,
                                const std::atomic_bool& cancel) override {
    {
      std::lock_guard lock(mutex_); active_ = true;
    }
    changed_.notify_all();
    std::unique_lock lock(mutex_);
    changed_.wait(lock, [&] { return released_ || cancel.load(); });
    return {request.id, request.prompt, cancel.load()};
  }
  void wait_active() { std::unique_lock lock(mutex_); changed_.wait(lock, [&] { return active_; }); }
  void release() { { std::lock_guard lock(mutex_); released_ = true; } changed_.notify_all(); }
 private:
  bool ready_{true}; bool active_{}; bool released_{}; std::mutex mutex_; std::condition_variable changed_;
};
#ifdef _WIN32
class RecordingRuntime final : public jarvis::IOmniRuntime {
 public:
  void load(std::string) override { ready_ = true; }
  void unload() noexcept override { ready_ = false; }
  bool ready() const noexcept override { return ready_; }
  jarvis::InferenceResult infer(const jarvis::InferenceRequest& request,
                                const std::atomic_bool&) override {
    std::size_t perception_index = 0;
    {
      std::lock_guard lock(mutex_);
      contexts_[request.id] = bool(request.frame) && bool(request.audio_16khz_mono);
      frame_contexts_[request.id] = bool(request.frame);
      audio_contexts_[request.id] = bool(request.audio_16khz_mono);
      audible_contexts_[request.id] =
          request.audio_16khz_mono &&
          std::ranges::any_of(*request.audio_16khz_mono,
                              [](float sample) { return sample != 0.0F; });
      prompts_[request.id] = request.prompt;
      max_output_tokens_[request.id] = request.max_output_tokens;
      if (request.prompt.find("实时感知器") != std::string::npos ||
          request.prompt.find("低延迟游戏感知器") != std::string::npos) {
        perception_index = ++unified_perception_count_;
      }
    }
    changed_.notify_all();
    if (perception_index == 1) {
      return {request.id,
              R"({"scene":"game","confidence":0.9,"scene_evidence":{"game_surface":true,"interactive_gameplay":true},"observation":"角色残血从河道撤退，右侧敌人正在追击，两个技能仍在冷却","barrage_candidates":["长官，残血还在河道晃，嫌命长？","右边都追上来了，还不回头？","两个技能全黑还想反打，长官)",
              false};
    }
    if (perception_index == 3) {
      return {request.id,
              R"({"scene":"game","confidence":0.91,"scene_evidence":{"game_surface":true,"interactive_gameplay":true,"game_video_or_stream":false,"fullscreen_game_media":false,"non_game_surface":false})",
              false};
    }
    return {request.id,
            R"({"scene":"game","confidence":0.92,"scene_evidence":{"game_surface":true,"interactive_gameplay":true,"game_video_or_stream":false,"fullscreen_game_media":false,"active_instruction":false,"course_surface":false,"instructional_audio":false,"ordinary_browsing":false,"non_game_surface":false},"observation":"玩家继续推进并观察资源","barrage_candidates":["长官，路线清楚了，稳住推进","长官，路线清楚了，稳住推进","资源够用，这波节奏别断","视野打开了，先盯住侧面"],"course_transcript":"","course_note":"","course_title":"","course_interaction":"","capture_keyframe":false,"keyframe_note":"","assistant_message":""})",
            false};
  }
  bool start_duplex(std::string instruction) override {
    std::lock_guard lock(mutex_);
    duplex_instruction_ = std::move(instruction);
    duplex_instructions_.push_back(duplex_instruction_);
    ++duplex_start_count_;
    duplex_active_ = true;
    changed_.notify_all();
    return true;
  }
  void stop_duplex() noexcept override {
    {
      std::lock_guard lock(mutex_);
      duplex_active_ = false;
    }
    changed_.notify_all();
  }
  bool duplex_active() const noexcept override { return duplex_active_.load(); }
  bool push_duplex(jarvis::DuplexFrame frame) override {
    std::lock_guard lock(mutex_);
    if (!duplex_active_) return false;
    duplex_had_context_ = bool(frame.frame) && bool(frame.audio_16khz_mono) &&
                          frame.audio_16khz_mono->size() == 16'000;
    duplex_results_.push_back({frame.sequence, true, frame.sequence == 2,
                               frame.sequence == 2 ? "测试条件已满足" : "", 5.0});
    changed_.notify_all();
    return true;
  }
  std::optional<jarvis::DuplexResult> wait_duplex(
      std::chrono::milliseconds timeout) override {
    std::unique_lock lock(mutex_);
    changed_.wait_for(lock, timeout, [&] {
      return !duplex_results_.empty() || !duplex_active_;
    });
    if (duplex_results_.empty()) return std::nullopt;
    auto result = std::move(duplex_results_.front());
    duplex_results_.pop_front();
    return result;
  }
  void wait_request(std::uint64_t id) {
    std::unique_lock lock(mutex_);
    changed_.wait_for(lock, std::chrono::seconds(2), [&] { return contexts_.contains(id); });
  }
  bool had_context(std::uint64_t id) {
    std::lock_guard lock(mutex_); return contexts_.contains(id) && contexts_[id];
  }
  bool received_audible_perception() {
    std::lock_guard lock(mutex_);
    return std::ranges::any_of(audible_contexts_, [](const auto& item) {
      return item.first >= (std::uint64_t{1} << 63U) && item.second;
    });
  }
  bool received_perception() {
    std::lock_guard lock(mutex_);
    bool isolated_profile_on_initial_classification = false;
    bool profiled_game_continuity = false;
    std::size_t unified_prompt_bytes{};
    std::size_t compact_game_prompt_bytes{};
    for (const auto& [id, prompt] : prompts_) {
      if (id < (std::uint64_t{1} << 63U)) continue;
      if (prompt.find("实时感知器") != std::string::npos &&
          prompt.find("场景判定") != std::string::npos &&
          prompt.find("scene_evidence") != std::string::npos &&
          prompt.find("game_surface") != std::string::npos &&
          prompt.find("non_game_surface") != std::string::npos &&
          prompt.find("fullscreen_game_media") != std::string::npos &&
          prompt.find("course_interaction") != std::string::npos &&
          prompt.find("当前证据优先") != std::string::npos &&
          prompt.find("其他生成字段只能使用 observation 中的事实") !=
              std::string::npos &&
          prompt.find("至少两项一致锚点") != std::string::npos &&
          prompt.find("互动游戏直接依据当前帧") != std::string::npos &&
          prompt.find("启动器、商店、游戏库") != std::string::npos &&
          prompt.find("静态课件与授课音频主题一致") != std::string::npos &&
          prompt.find("判断、态度、提醒、建议或克制吐槽") !=
              std::string::npos &&
          prompt.find("普通视频或直播的回复由全双工通道负责") !=
              std::string::npos &&
          prompt.find("不要因画面切换而强行发言") != std::string::npos &&
          prompt.find("用户正在做什么") !=
              std::string::npos &&
          prompt.find("页面上有什么") !=
              std::string::npos &&
          prompt.find("barrage_candidates") != std::string::npos &&
          prompt.find("\"observation\":\"\"") <
              prompt.find("\"barrage_candidates\":[]") &&
          prompt.find("<game_profile>") == std::string::npos &&
          prompt.find("本轮游戏弹幕主角度") == std::string::npos &&
          prompt.size() < 7000 &&
          prompt.find(std::string(5000, 'x')) == std::string::npos) {
        isolated_profile_on_initial_classification = true;
        unified_prompt_bytes = prompt.size();
      }
      if (prompt.find("低延迟游戏感知器") != std::string::npos &&
          prompt.find("一次推理直接返回合法 JSON") != std::string::npos &&
          prompt.find("不等待后续时间片") != std::string::npos &&
          prompt.find("互动游戏直接使用当前帧") != std::string::npos &&
          prompt.find("所有生成内容只能使用这些事实") !=
              std::string::npos &&
          prompt.find("互动游戏必须设置前两个 game 键") !=
              std::string::npos &&
          prompt.find("恰好 3 条非空") != std::string::npos &&
          prompt.find("本轮游戏弹幕主角度") == std::string::npos &&
          prompt.find("<game_profile>专业毒舌嘴臭教练") != std::string::npos &&
          prompt.find("中间的重复或次要要求已压缩") != std::string::npos &&
          prompt.find("结尾必须称呼长官</game_profile>") != std::string::npos &&
          prompt.find(std::string(5000, 'x')) == std::string::npos) {
        compact_game_prompt_bytes = prompt.size();
        const auto profile_start = prompt.find("<game_profile>");
        const auto profile_end = prompt.find("</game_profile>", profile_start);
        if (profile_start != std::string::npos &&
            profile_end != std::string::npos) {
          compact_game_prompt_bytes -=
              profile_end + std::string_view("</game_profile>").size() -
              profile_start;
        }
        profiled_game_continuity = compact_game_prompt_bytes < 3500;
      }
    }
    return isolated_profile_on_initial_classification &&
           profiled_game_continuity && unified_prompt_bytes > 0 &&
           compact_game_prompt_bytes > 0 &&
           compact_game_prompt_bytes < unified_prompt_bytes;
  }
  bool unified_perception_uses_multimodal_context() {
    std::lock_guard lock(mutex_);
    for (const auto& [id, prompt] : prompts_) {
      if (id >= (std::uint64_t{1} << 63U) &&
          prompt.find("实时感知器") != std::string::npos &&
          frame_contexts_.contains(id) && frame_contexts_[id] &&
          audio_contexts_.contains(id) && audio_contexts_[id] &&
          max_output_tokens_.contains(id) && max_output_tokens_[id] == 0) {
        return true;
      }
    }
    return false;
  }
  std::size_t perception_request_count() {
    std::lock_guard lock(mutex_);
    return std::count_if(prompts_.begin(), prompts_.end(), [](const auto& item) {
      return item.first >= (std::uint64_t{1} << 63U);
    });
  }
  bool received_duplex_context() {
    std::lock_guard lock(mutex_);
    return duplex_had_context_ && duplex_instruction_.find("绿灯") != std::string::npos;
  }
  void wait_for_duplex_rebuild() {
    std::unique_lock lock(mutex_);
    changed_.wait_for(lock, std::chrono::seconds(2), [&] {
      return duplex_start_count_ >= 2;
    });
  }
  bool duplex_rebuilt_with_same_instruction() {
    std::lock_guard lock(mutex_);
    return duplex_instructions_.size() >= 2 &&
           std::ranges::all_of(duplex_instructions_, [&](const auto& instruction) {
             return instruction == duplex_instructions_.front();
           });
  }
 private:
  bool ready_{};
  std::unordered_map<std::uint64_t, bool> contexts_;
  std::unordered_map<std::uint64_t, bool> frame_contexts_;
  std::unordered_map<std::uint64_t, bool> audio_contexts_;
  std::unordered_map<std::uint64_t, bool> audible_contexts_;
  std::unordered_map<std::uint64_t, std::string> prompts_;
  std::unordered_map<std::uint64_t, std::int32_t> max_output_tokens_;
  std::size_t unified_perception_count_{};
  std::deque<jarvis::DuplexResult> duplex_results_;
  std::string duplex_instruction_;
  std::vector<std::string> duplex_instructions_;
  std::size_t duplex_start_count_{};
  std::atomic_bool duplex_active_{false};
  bool duplex_had_context_{};
  std::mutex mutex_; std::condition_variable changed_;
};
class TestDesktopCapture final : public jarvis::IDesktopCapture {
 public:
  void start() override {}
  void stop() noexcept override {}
  std::optional<jarvis::VideoFrame> next_frame(std::uint32_t) override {
    std::vector<std::byte> pixels(16, (++sequence_ % 2) ? std::byte{0} : std::byte{255});
    return jarvis::VideoFrame{2, 2, 8, 0, std::move(pixels)};
  }
 private:
  std::size_t sequence_{};
};
class TestAudioCapture final : public jarvis::IAudioCapture {
 public:
  void start() override {}
  void stop() noexcept override {}
  std::optional<jarvis::audio::PcmBlock> next_block(std::uint32_t) override {
    return jarvis::audio::PcmBlock{{16'000, 1}, std::vector<float>(320, 0.01F), 0};
  }
};
#endif
}
int main() {
  using namespace jarvis;
  const char text[] = "hello";
  auto encoded = ipc::encode(ipc::MessageType::submit, 42, bytes(text, 5));
  auto decoded = ipc::decode(encoded);
  require(bool(decoded), "protocol round trip");
  require(decoded.message.header.request_id == 42 && decoded.message.payload.size() == 5, "protocol fields");
  encoded.back() ^= std::byte{1}; require(!ipc::decode(encoded), "protocol detects corruption");

  const float stereo[] = {1, -1, .5F, .5F};
  auto mono = audio::downmix_mono(stereo, 2);
  require(mono.size() == 2 && mono[0] == 0 && mono[1] == .5F, "PCM downmix");
  auto up = audio::resample_linear(mono, 2, 4); require(up.size() == 4, "PCM resampling size");
  audio::ExactWindowAssembler windows(10, 2'000);
  float samples[45]{}; auto first = windows.push(std::span(samples, 15)); require(first.empty(), "partial window held");
  auto second = windows.push(std::span(samples + 15, 30)); require(second.size() == 2 && second[0].size() == 20, "exact windows");

  VideoFrame frame{2, 2, 8, 0, std::vector<std::byte>(16)};
  FrameDeduplicator dedupe; require(dedupe.changed(frame), "first frame changes"); require(!dedupe.changed(frame), "duplicate frame ignored");
  frame.bgra[0] = std::byte{255}; require(dedupe.changed(frame), "changed frame detected");
  VideoFrame large_frame{64, 36, 256, 0, std::vector<std::byte>(64 * 36 * 4)};
  FrameChangeDetector visual_changes;
  require(visual_changes.changed(large_frame), "first visual sample changes");
  large_frame.bgra[0] = std::byte{255};
  require(!visual_changes.changed(large_frame), "localized pixel noise is ignored");
  for (std::size_t sample = 1; sample <= 4; ++sample) {
    const auto at = sample * 2 * 4;
    large_frame.bgra[at] = std::byte{255};
    large_frame.bgra[at + 1] = std::byte{255};
    large_frame.bgra[at + 2] = std::byte{255};
  }
  require(!visual_changes.changed(large_frame), "small changes accumulate below threshold");
  for (std::size_t sample = 5; sample <= 8; ++sample) {
    const auto at = sample * 2 * 4;
    large_frame.bgra[at] = std::byte{255};
    large_frame.bgra[at + 1] = std::byte{255};
    large_frame.bgra[at + 2] = std::byte{255};
  }
  require(visual_changes.changed(large_frame), "accumulated visual change is detected");
  for (std::size_t index = 0; index < large_frame.bgra.size() / 2; index += 4) {
    large_frame.bgra[index] = std::byte{255};
    large_frame.bgra[index + 1] = std::byte{255};
    large_frame.bgra[index + 2] = std::byte{255};
  }
  require(visual_changes.changed(large_frame), "broad visual change is detected");

  const auto idle_start = std::chrono::steady_clock::time_point{};
  ScreenIdleMonitor idle_screen(std::chrono::seconds(120),
                                std::chrono::seconds(60),
                                std::chrono::seconds(120), 7);
  require(idle_screen.observe(true, idle_start) == ScreenIdleEvent::none,
          "first changed frame initializes idle tracking");
  require(idle_screen.observe(false, idle_start + std::chrono::seconds(119)) ==
              ScreenIdleEvent::none,
          "screen remains active before two minutes");
  require(idle_screen.observe(false, idle_start + std::chrono::seconds(120)) ==
              ScreenIdleEvent::entered_idle &&
              idle_screen.idle(),
          "unchanged screen enters idle at two minutes");
  auto first_reminder_at = 0;
  for (auto second = 121; second <= 240; ++second) {
    if (idle_screen.observe(false, idle_start + std::chrono::seconds(second)) ==
        ScreenIdleEvent::reminder_due) {
      first_reminder_at = second;
      break;
    }
  }
  require(first_reminder_at >= 180 && first_reminder_at <= 240,
          "idle reminder delay is randomized between 60 and 120 seconds");
  auto second_reminder_at = 0;
  for (auto second = first_reminder_at + 1; second <= first_reminder_at + 120;
       ++second) {
    if (idle_screen.observe(false, idle_start + std::chrono::seconds(second)) ==
        ScreenIdleEvent::reminder_due) {
      second_reminder_at = second;
      break;
    }
  }
  require(second_reminder_at - first_reminder_at >= 60 &&
              second_reminder_at - first_reminder_at <= 120,
          "each repeated idle reminder gets a new bounded delay");
  require(idle_screen.observe(true, idle_start + std::chrono::seconds(400)) ==
              ScreenIdleEvent::resumed &&
              !idle_screen.idle(),
          "screen change exits idle immediately");

  auto runtime = make_stub_omni_runtime(); runtime->load("stub");
  std::mutex mutex; std::vector<InferenceResult> results;
  LatestOnlyScheduler scheduler(*runtime, [&](InferenceResult r) { std::lock_guard lock(mutex); results.push_back(std::move(r)); });
  scheduler.start(); scheduler.submit({InferenceRequest{.id=7, .prompt="test"}, Priority::interactive});
  for (int i = 0; i < 100 && scheduler.busy(); ++i) std::this_thread::sleep_for(std::chrono::milliseconds(2));
  scheduler.stop();
  { std::lock_guard lock(mutex); require(!results.empty() && results.back().id == 7, "scheduler invokes runtime"); }

  BlockingRuntime blocking; std::vector<InferenceResult> ordered; std::mutex ordered_mutex;
  LatestOnlyScheduler coalescing(blocking, [&](InferenceResult r) {
    std::lock_guard lock(ordered_mutex); ordered.push_back(std::move(r));
  });
  coalescing.start();
  coalescing.submit({InferenceRequest{.id=10, .prompt="active"}, Priority::normal});
  blocking.wait_active();
  coalescing.submit({InferenceRequest{.id=11, .prompt="interactive"}, Priority::interactive});
  coalescing.submit({InferenceRequest{.id=12, .prompt="rejected background"}, Priority::background});
  blocking.release();
  for (int i = 0; i < 100; ++i) {
    {
      std::lock_guard lock(ordered_mutex);
      if (ordered.size() == 1 && ordered.front().id == 11) break;
    }
    std::this_thread::sleep_for(std::chrono::milliseconds(2));
  }
  coalescing.stop();
  {
    std::lock_guard lock(ordered_mutex);
    require(ordered.size() == 1, "cancelled stale and rejected work do not add completion");
    require(ordered[0].id == 11, "accepted interactive request remains pending");
  }
#ifdef _WIN32
  auto recording = std::make_unique<RecordingRuntime>();
  auto* recording_ptr = recording.get();
  Worker worker(std::move(recording));
  std::mutex native_event_mutex;
  std::vector<std::string> native_events;
  std::vector<std::string> perception_results;
  worker.set_completion([&](InferenceResult result) {
    std::lock_guard lock(native_event_mutex);
    if (result.id == std::numeric_limits<std::uint64_t>::max()) {
      native_events.push_back(std::move(result.text));
    } else if (result.id >= (std::uint64_t{1} << 63U)) {
      perception_results.push_back(std::move(result.text));
    }
  });
  require(worker.start("test"), "worker starts with recording runtime");
  std::string long_game_profile = "专业毒舌嘴臭教练，称呼我为“长官”。";
  long_game_profile += std::string(5000, 'x');
  long_game_profile += "结尾必须称呼长官";
  worker.set_game_profile("任意测试游戏", std::move(long_game_profile));
  require(worker.start_monitoring(std::make_unique<TestDesktopCapture>(),
                                  std::make_unique<TestAudioCapture>(),
                                  std::chrono::milliseconds(10)),
          "monitoring starts with capture devices");
  std::this_thread::sleep_for(std::chrono::milliseconds(2300));
  require(recording_ptr->received_perception(), "monitoring schedules structured perception");
  {
    std::lock_guard lock(native_event_mutex);
    require(!perception_results.empty() &&
                std::ranges::all_of(perception_results, has_unified_perception_schema),
            "every perception result uses the fixed cross-scene JSON schema");
    require(std::ranges::any_of(perception_results, [](const auto& event) {
              return event.find("角色残血从河道撤退") !=
                         std::string::npos &&
                     event.find("长官，残血还在河道晃，嫌命长？") !=
                         std::string::npos &&
                     event.find("右边都追上来了，还不回头？") !=
                         std::string::npos &&
                     event.find("\"barrage_pending\":false") != std::string::npos &&
                     event.find("\"classification_recovered\":true") !=
                         std::string::npos &&
                     event.find("\"barrage_source\":\"model\"") !=
                         std::string::npos &&
                     event.find("\"barrage_fallback_reason\":\"\"") !=
                         std::string::npos;
            }),
            "complete candidates survive a partially truncated candidate array");
    require(std::ranges::any_of(perception_results, [](const auto& event) {
              return event.find("\"classification_recovered\":true") !=
                         std::string::npos &&
                     event.find("\"barrage_source\":\"fallback\"") !=
                         std::string::npos &&
                     event.find("\"barrage_fallback_reason\":\"truncated_output\"") !=
                         std::string::npos &&
                     event.find("\"barrage_candidates\":[\"") !=
                         std::string::npos &&
                     event.find("\"barrage_candidates\":[\"\"]") ==
                         std::string::npos;
            }),
            "truncated output without candidates still uses a fallback");
  }
  {
    std::lock_guard lock(native_event_mutex);
    require(std::ranges::any_of(perception_results, [](const auto& event) {
              return event.find(
                         "\"barrage_candidates\":[\"长官，路线清楚了，稳住推进\","
                         "\"资源够用，这波节奏别断\",\"视野打开了，先盯住侧面\"]") !=
                         std::string::npos &&
                     event.find("\"barrage_source\":\"model\"") !=
                         std::string::npos;
            }),
            "the next game result deduplicates model-generated candidates");
  }
  require(recording_ptr->received_audible_perception(),
          "structured perception receives audible system audio");
  require(recording_ptr->perception_request_count() == 3,
          "one-second perception cadence runs three unified model requests");
  require(recording_ptr->unified_perception_uses_multimodal_context(),
          "unified perception receives the current frame and system audio");
  require(worker.start_duplex("traffic-light", "持续观察画面，绿灯亮起时提醒我"),
          "duplex task starts while monitoring remains active");
  std::this_thread::sleep_for(std::chrono::milliseconds(40));
  require(recording_ptr->received_duplex_context(),
          "duplex task receives one-second audio and current frame");
  {
    std::lock_guard lock(native_event_mutex);
    require(std::ranges::any_of(native_events, [](const auto& event) {
              return event.find("\"decision\":\"listen\"") != std::string::npos;
            }),
            "duplex task emits model listen decisions");
    require(std::ranges::any_of(native_events, [](const auto& event) {
              return event.find("\"decision\":\"speak\"") != std::string::npos &&
                     event.find("测试条件已满足") != std::string::npos;
            }),
            "duplex task emits model speak decisions with text");
  }
  require(recording_ptr->perception_request_count() == 3,
          "duplex task does not replace structured perception");
  recording_ptr->wait_for_duplex_rebuild();
  require(recording_ptr->duplex_rebuilt_with_same_instruction(),
          "duplex context is periodically rebuilt with the original instruction");
  for (int i = 0; i < 100; ++i) {
    {
      std::lock_guard lock(native_event_mutex);
      if (std::ranges::any_of(native_events, [](const auto& event) {
            return event.find("\"native_event\":\"duplex.rebuilt\"") !=
                   std::string::npos;
          })) {
        break;
      }
    }
    std::this_thread::sleep_for(std::chrono::milliseconds(2));
  }
  {
    std::lock_guard lock(native_event_mutex);
    require(std::ranges::any_of(native_events, [](const auto& event) {
              return event.find("\"native_event\":\"duplex.rebuild.requested\"") !=
                         std::string::npos &&
                     event.find("\"completed_frames\":24") !=
                         std::string::npos;
            }),
            "duplex context rebuild is requested at the safe horizon");
    require(std::ranges::any_of(native_events, [](const auto& event) {
              return event.find("\"native_event\":\"duplex.rebuilt\"") !=
                     std::string::npos;
            }),
            "duplex context rebuild emits a completion event");
  }
  worker.stop_duplex();
  worker.submit_prompt(21, "describe context");
  recording_ptr->wait_request(21);
  require(recording_ptr->had_context(21), "interactive prompt includes latest capture context");
  worker.submit_prompt(23, "[[JARVIS_TEXT_ONLY]]\nsummarize transcript");
  recording_ptr->wait_request(23);
  require(!recording_ptr->had_context(23), "text-only prompt excludes capture context");
  worker.stop_monitoring();
  worker.submit_prompt(22, "text only");
  recording_ptr->wait_request(22);
  require(!recording_ptr->had_context(22), "stopping monitoring clears capture context");
  worker.stop();
#endif
  std::cout << "all native tests passed\n";
}
