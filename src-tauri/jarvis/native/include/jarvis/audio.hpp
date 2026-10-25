#pragma once

#include <cstddef>
#include <cstdint>
#include <span>
#include <vector>

namespace jarvis::audio {

struct Format {
  std::uint32_t sample_rate{48'000};
  std::uint16_t channels{2};
};

struct PcmBlock {
  Format format{};
  std::vector<float> interleaved{};
  std::uint64_t timestamp_100ns{};
};

[[nodiscard]] std::vector<float> downmix_mono(std::span<const float> interleaved,
                                               std::uint16_t channels);
[[nodiscard]] std::vector<float> resample_linear(std::span<const float> mono,
                                                  std::uint32_t input_rate,
                                                  std::uint32_t output_rate);

class ExactWindowAssembler {
 public:
  ExactWindowAssembler(std::uint32_t sample_rate = 16'000, std::uint32_t milliseconds = 2'000);
  [[nodiscard]] std::vector<std::vector<float>> push(std::span<const float> samples);
  void reset() noexcept;
  [[nodiscard]] std::size_t window_samples() const noexcept { return window_samples_; }
  [[nodiscard]] std::size_t buffered_samples() const noexcept { return buffer_.size() - offset_; }

 private:
  std::size_t window_samples_{};
  std::vector<float> buffer_{};
  std::size_t offset_{};
};

} // namespace jarvis::audio
