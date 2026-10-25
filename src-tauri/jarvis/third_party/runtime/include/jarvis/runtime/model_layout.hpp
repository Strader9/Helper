#pragma once

#include <cstdint>
#include <filesystem>
#include <string>
#include <vector>

namespace jarvis::runtime {

struct ModelFile {
  std::filesystem::path path;
  std::uintmax_t size{};
};

struct ModelLayout {
  ModelFile llm;
  ModelFile vpm;
  ModelFile apm;
};

struct ValidationResult {
  ModelLayout files;
  std::vector<std::string> errors;

  [[nodiscard]] bool ok() const noexcept { return errors.empty(); }
};

// Validates the text-output MiniCPM-o runtime inputs. This validates identity,
// placement, readability, and the GGUF magic only; it does not authenticate
// model contents. Deployments should additionally pin model SHA-256 values.
[[nodiscard]] ValidationResult validate_minicpm_o_4_5_layout(
    const std::filesystem::path& model_root,
    const std::filesystem::path& llm_filename = "MiniCPM-o-4_5-Q4_K_M.gguf");

}  // namespace jarvis::runtime
