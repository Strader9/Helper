# Third-party notices

AI Jarvis bundles third-party software under its respective licenses. This
notice is informational and does not replace the license texts shipped by the
individual components.

## Desktop runtime

- Electron, including Chromium and Node.js: MIT, BSD-style, and component
  licenses distributed with Electron.
- `ws`: MIT License.
- Lucide: ISC License.

## Python runtime

- FastAPI: MIT License.
- Uvicorn: BSD 3-Clause License.
- Pydantic and pydantic-settings: MIT License.
- Hugging Face Hub: Apache License 2.0.
- `hf-xet`: Apache License 2.0.
- tqdm: MPL 2.0 and MIT License.
- PyInstaller bootloader: GPL 2.0 or later with the PyInstaller bootloader
  exception.

## Native inference runtime

The native provider is based on `llama.cpp-omni`, copyright 2023-2026 the
ggml authors, under the MIT License. The pinned upstream revision and patch
inventory are recorded in `third_party/runtime/VENDOR.json`. Its preserved
license is in `third_party/runtime/LICENSE.llama.cpp-omni`; additional upstream
license texts are retained under `third_party/runtime/vendor/licenses`.

## Model weights

Model weights are not included in the installer. On first start, the app
downloads the pinned MiniCPM-o 4.5 GGUF files from
`openbmb/MiniCPM-o-4_5-gguf`. Those files remain subject to the model
publisher's license and usage terms.
