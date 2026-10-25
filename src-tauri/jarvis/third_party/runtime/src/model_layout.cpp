#include "jarvis/runtime/model_layout.hpp"

#include <array>
#include <fstream>
#include <system_error>

namespace jarvis::runtime {
namespace {

constexpr std::array<char, 4> kGgufMagic{'G', 'G', 'U', 'F'};

ModelFile inspect_gguf(const std::filesystem::path& path,
                       const char* role,
                       std::vector<std::string>& errors) {
  ModelFile result{path, 0};
  std::error_code ec;
  if (!std::filesystem::is_regular_file(path, ec)) {
    errors.emplace_back(std::string(role) + " model is not a regular file: " +
                        path.string());
    return result;
  }

  result.size = std::filesystem::file_size(path, ec);
  if (ec || result.size < kGgufMagic.size()) {
    errors.emplace_back(std::string(role) + " model is unreadable or truncated: " +
                        path.string());
    return result;
  }

  std::ifstream input(path, std::ios::binary);
  std::array<char, 4> magic{};
  if (!input.read(magic.data(), static_cast<std::streamsize>(magic.size())) ||
      magic != kGgufMagic) {
    errors.emplace_back(std::string(role) + " model does not have GGUF magic: " +
                        path.string());
  }
  return result;
}

}  // namespace

ValidationResult validate_minicpm_o_4_5_layout(
    const std::filesystem::path& model_root,
    const std::filesystem::path& llm_filename) {
  ValidationResult result;
  std::error_code ec;
  if (!std::filesystem::is_directory(model_root, ec)) {
    result.errors.emplace_back("model root is not a directory: " + model_root.string());
    return result;
  }
  if (llm_filename.empty() || llm_filename.is_absolute() ||
      llm_filename.has_parent_path()) {
    result.errors.emplace_back("LLM filename must be one filename relative to model root");
    return result;
  }

  result.files.llm = inspect_gguf(model_root / llm_filename, "LLM", result.errors);
  result.files.vpm = inspect_gguf(
      model_root / "vision" / "MiniCPM-o-4_5-vision-F16.gguf", "VPM", result.errors);
  result.files.apm = inspect_gguf(
      model_root / "audio" / "MiniCPM-o-4_5-audio-F16.gguf", "APM", result.errors);
  return result;
}

}  // namespace jarvis::runtime
