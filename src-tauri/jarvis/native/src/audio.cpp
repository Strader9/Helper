#include "jarvis/audio.hpp"

#include <algorithm>
#include <cmath>
#include <stdexcept>

namespace jarvis::audio {

std::vector<float> downmix_mono(std::span<const float> input, std::uint16_t channels) {
  if (channels == 0 || input.size() % channels != 0) throw std::invalid_argument("invalid channel layout");
  std::vector<float> output(input.size() / channels);
  for (std::size_t frame = 0; frame < output.size(); ++frame) {
    double sum{};
    for (std::uint16_t channel = 0; channel < channels; ++channel) sum += input[frame * channels + channel];
    output[frame] = std::clamp(static_cast<float>(sum / channels), -1.0F, 1.0F);
  }
  return output;
}

std::vector<float> resample_linear(std::span<const float> input, std::uint32_t input_rate,
                                   std::uint32_t output_rate) {
  if (!input_rate || !output_rate) throw std::invalid_argument("sample rate must be non-zero");
  if (input.empty()) return {};
  const auto count = static_cast<std::size_t>(std::llround(double(input.size()) * output_rate / input_rate));
  std::vector<float> output(count);
  const double step = double(input_rate) / output_rate;
  for (std::size_t i = 0; i < count; ++i) {
    const double source = i * step;
    const auto left = std::min(static_cast<std::size_t>(source), input.size() - 1);
    const auto right = std::min(left + 1, input.size() - 1);
    const float fraction = static_cast<float>(source - left);
    output[i] = input[left] + (input[right] - input[left]) * fraction;
  }
  return output;
}

ExactWindowAssembler::ExactWindowAssembler(std::uint32_t rate, std::uint32_t milliseconds)
    : window_samples_(static_cast<std::size_t>((std::uint64_t(rate) * milliseconds) / 1000U)) {
  if (!window_samples_) throw std::invalid_argument("window must contain samples");
}

std::vector<std::vector<float>> ExactWindowAssembler::push(std::span<const float> samples) {
  buffer_.insert(buffer_.end(), samples.begin(), samples.end());
  std::vector<std::vector<float>> windows;
  while (buffer_.size() - offset_ >= window_samples_) {
    windows.emplace_back(buffer_.begin() + static_cast<std::ptrdiff_t>(offset_),
                         buffer_.begin() + static_cast<std::ptrdiff_t>(offset_ + window_samples_));
    offset_ += window_samples_;
  }
  if (offset_ && (offset_ == buffer_.size() || offset_ > window_samples_ * 2)) {
    buffer_.erase(buffer_.begin(), buffer_.begin() + static_cast<std::ptrdiff_t>(offset_)); offset_ = 0;
  }
  return windows;
}
void ExactWindowAssembler::reset() noexcept { buffer_.clear(); offset_ = 0; }
} // namespace jarvis::audio
