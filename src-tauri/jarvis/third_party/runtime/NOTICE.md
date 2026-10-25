# Third-party notices

This boundary is independently maintained by the AI Jarvis project. The
standalone boundary target performs model layout validation without compiling
upstream inference code. Real builds explicitly enable the vendored, pinned
upstream source and its checksum-pinned text-input patch.

The optional provider is based on **llama.cpp-omni**, copyright 2023-2026 the
ggml authors, distributed under the MIT License. Canonical source and exact
revision are recorded in `VENDOR.json`; the license text is preserved in
`LICENSE.llama.cpp-omni`.

llama.cpp-omni is based on llama.cpp and carries additional notices and
licenses in its own pinned source tree. Distributions that include the provider
must retain the pinned tree's `LICENSE`, `AUTHORS`, and `licenses/` contents,
review licenses for enabled backends, and regenerate this notice inventory.
Model weights are not source code and have separate licenses; no model weights
are included here.
