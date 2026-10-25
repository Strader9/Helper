# Native runtime integration contract

## Implemented boundary

`jarvis::runtime_boundary` is a normal CMake target with no network or
inference dependency. It validates the required MiniCPM-o 4.5 text-output
model layout and GGUF file signatures. It can be integrated with:

```cmake
add_subdirectory(third_party/runtime)
target_link_libraries(your_adapter PRIVATE jarvis::runtime_boundary)
```

This is deliberately not an inference stub. A successful validation means
only that plausible input files are present; it never claims they can run.

## Vendored source

`vendor/` contains the archive identified by `VENDOR.json`, extracted only after
its SHA-256 matched the manifest. It is a real, independent source snapshot and
contains no symlink or path dependency on reference material. Production builds
use the checksum-pinned patch to expose the upstream `omni` library through the
stable `omni_text_runtime` target.

## Expected model layout

The root supplied to `validate_minicpm_o_4_5_layout` must contain:

```text
MiniCPM-o-4_5-gguf/
  MiniCPM-o-4_5-Q4_K_M.gguf       # configurable filename; LLM
  vision/
    MiniCPM-o-4_5-vision-F16.gguf # VPM
  audio/
    MiniCPM-o-4_5-audio-F16.gguf  # APM, 16 kHz mono input
```

F16 and Q8_0 LLM variants may be selected by passing their filename. TTS,
semantic projector, token2wav, voice cloning, and output audio assets are not
accepted or required. Model weights are acquired separately under their own
license. Production packaging should pin SHA-256 for every model file; names,
size, and GGUF magic are not cryptographic verification.

## Source acquisition

`VENDOR.json` is authoritative. Download exactly its immutable archive URL,
verify its SHA-256, and extract into a build cache outside this repository.
Do not use a moving branch, a symlink, or a source directory from another
project checkout. Normal project configuration never fetches source and
offline builds use the checked-in `vendor/` snapshot.

For independently refreshing an external cache, `cmake/AcquireUpstream.cmake`
tries the immutable official URL first, then the configured GitHub mirror after
an inactive connection times out. Both paths must match the SHA-256 in
`VENDOR.json`; set `JARVIS_DISABLE_DOWNLOAD_MIRROR=1` to require the official
source only.

Example configuration after acquisition:

```text
cmake -S . -B build \
  -DJARVIS_RUNTIME_ENABLE_UPSTREAM=ON \
  -DJARVIS_RUNTIME_UPSTREAM_SOURCE_DIR=/absolute/cache/llama.cpp-omni-<revision>
```

## Patch policy and inference provider

Patches belong in `patches/`, are applied in lexical order with `git apply`,
and must be listed with SHA-256 values in `VENDOR.json`. Patches must carry an
upstream revision header, attribution, rationale, and update note. Generated
or unreviewed source snapshots are not accepted.

At the pinned revision, upstream's single `omni` library includes TTS condition
and token2wav sources in the same compilation unit as LLM/VPM/APM. The pinned
patch exposes that library as `omni_text_runtime`; the Jarvis adapter calls
`omni_init(..., use_tts=false, ...)`, does not load TTS/projector/token2wav
weights, and returns text only. This retains some unused speech code in the
binary but does not use fake inference or load the speech models.

The native adapter implements `IOmniRuntime`, maps video to VPM and 16 kHz mono
float audio to APM through temporary media files, returns text only, checks
cancellation at request boundaries, and owns all upstream state in its private
implementation. Upstream headers do not leak into the project's public API.
