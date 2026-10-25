#ifdef _WIN32
#include "jarvis/windows.hpp"

#include <Windows.h>
#include <Audioclient.h>
#include <Mmdeviceapi.h>
#include <avrt.h>
#include <ksmedia.h>
#include <wrl/client.h>

#include <algorithm>
#include <chrono>
#include <cstring>
#include <stdexcept>
#include <thread>

namespace jarvis::win {
using Microsoft::WRL::ComPtr;
namespace {
void check(HRESULT hr, const char* what) { if (FAILED(hr)) throw std::runtime_error(what); }
class WasapiLoopbackCapture final : public IAudioCapture {
 public:
  void start() override {
    if (capture_) return;
    try {
      const auto co = CoInitializeEx(nullptr, COINIT_MULTITHREADED);
      owns_com_ = SUCCEEDED(co);
      ComPtr<IMMDeviceEnumerator> enumerator;
      check(CoCreateInstance(__uuidof(MMDeviceEnumerator), nullptr, CLSCTX_ALL, IID_PPV_ARGS(&enumerator)), "MMDeviceEnumerator failed");
      check(enumerator->GetDefaultAudioEndpoint(eRender, eConsole, &device_), "default render endpoint failed");
      check(device_->Activate(__uuidof(IAudioClient), CLSCTX_ALL, nullptr, reinterpret_cast<void**>(client_.GetAddressOf())), "IAudioClient failed");
      check(client_->GetMixFormat(&mix_), "GetMixFormat failed");
      format_ = {mix_->nSamplesPerSec, mix_->nChannels};
      check(client_->Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK, 0, 0, mix_, nullptr), "loopback initialize failed");
      check(client_->GetService(IID_PPV_ARGS(&capture_)), "IAudioCaptureClient failed");
      check(client_->Start(), "audio start failed");
    } catch (...) { stop(); throw; }
  }
  void stop() noexcept override {
    if (client_) client_->Stop(); capture_.Reset(); client_.Reset(); device_.Reset();
    if (mix_) { CoTaskMemFree(mix_); mix_ = nullptr; }
    if (owns_com_) { CoUninitialize(); owns_com_ = false; }
  }
  std::optional<audio::PcmBlock> next_block(std::uint32_t timeout_ms) override {
    if (!capture_) throw std::runtime_error("audio capture not started");
    const auto deadline = std::chrono::steady_clock::now() + std::chrono::milliseconds(timeout_ms);
    UINT32 packets{};
    while (std::chrono::steady_clock::now() < deadline) {
      check(capture_->GetNextPacketSize(&packets), "GetNextPacketSize failed");
      if (packets) break;
      std::this_thread::sleep_for(std::chrono::milliseconds(2));
    }
    if (!packets) return std::nullopt;
    audio::PcmBlock block; block.format = format_; bool have_timestamp = false;
    while (packets) {
      BYTE* data{}; UINT32 frames{}; DWORD flags{}; UINT64 device_position{}, qpc_position{};
      check(capture_->GetBuffer(&data, &frames, &flags, &device_position, &qpc_position), "GetBuffer failed");
      const bool release_needed = true;
      try {
        const auto samples = std::size_t(frames) * format_.channels;
        const auto old = block.interleaved.size(); block.interleaved.resize(old + samples);
        if (flags & AUDCLNT_BUFFERFLAGS_SILENT) std::fill(block.interleaved.begin() + static_cast<std::ptrdiff_t>(old), block.interleaved.end(), 0.0F);
        else if (is_float()) std::memcpy(block.interleaved.data() + old, data, samples * sizeof(float));
        else if (mix_->wBitsPerSample == 16) {
          const auto* pcm = reinterpret_cast<const std::int16_t*>(data);
          for (std::size_t i = 0; i < samples; ++i) block.interleaved[old + i] = pcm[i] / 32768.0F;
        } else { throw std::runtime_error("unsupported WASAPI mix format"); }
        if (!have_timestamp && !(flags & AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR)) {
          block.timestamp_100ns = qpc_position; have_timestamp = true;
        }
      } catch (...) {
        if (release_needed) capture_->ReleaseBuffer(frames);
        throw;
      }
      check(capture_->ReleaseBuffer(frames), "ReleaseBuffer failed");
      check(capture_->GetNextPacketSize(&packets), "GetNextPacketSize failed");
    }
    return block;
  }
  ~WasapiLoopbackCapture() override { stop(); }
 private:
  bool is_float() const noexcept {
    if (mix_->wFormatTag == WAVE_FORMAT_IEEE_FLOAT) return true;
    if (mix_->wFormatTag != WAVE_FORMAT_EXTENSIBLE) return false;
    return reinterpret_cast<const WAVEFORMATEXTENSIBLE*>(mix_)->SubFormat == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT;
  }
  bool owns_com_{}; WAVEFORMATEX* mix_{}; audio::Format format_{};
  ComPtr<IMMDevice> device_; ComPtr<IAudioClient> client_; ComPtr<IAudioCaptureClient> capture_;
};
} // namespace
std::unique_ptr<IAudioCapture> make_wasapi_loopback_capture() { return std::make_unique<WasapiLoopbackCapture>(); }
} // namespace jarvis::win
#endif
