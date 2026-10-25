#ifdef _WIN32
#include "jarvis/windows.hpp"

#include <Windows.h>
#include <d3d11.h>
#include <dxgi1_2.h>
#include <wrl/client.h>

#include <chrono>
#include <cstring>
#include <iostream>
#include <sstream>
#include <stdexcept>

namespace jarvis::win {
using Microsoft::WRL::ComPtr;
namespace {
void check(HRESULT hr, const char* what) { if (FAILED(hr)) throw std::runtime_error(what); }
std::string hresult_message(const char* what, HRESULT hr) {
  std::ostringstream message;
  message << what << " (HRESULT=0x" << std::hex << static_cast<unsigned long>(hr) << ')';
  return message.str();
}
HMONITOR pet_monitor() noexcept {
  const auto pet_window = FindWindowW(nullptr, L"AI Jarvis Pet");
  return MonitorFromWindow(pet_window, MONITOR_DEFAULTTOPRIMARY);
}
class DxgiDesktopCapture final : public IDesktopCapture {
 public:
  void start() override {
    if (duplication_) return;
    ComPtr<IDXGIFactory1> factory;
    check(CreateDXGIFactory1(IID_PPV_ARGS(&factory)), "CreateDXGIFactory1 failed");
    HRESULT last_error = DXGI_ERROR_NOT_FOUND;
    const auto target_monitor = pet_monitor();
    for (int pass = 0; pass < 2; ++pass) {
      for (UINT adapter_index = 0;; ++adapter_index) {
        ComPtr<IDXGIAdapter1> adapter;
        if (factory->EnumAdapters1(adapter_index, &adapter) == DXGI_ERROR_NOT_FOUND) break;
        for (UINT output_index = 0;; ++output_index) {
          ComPtr<IDXGIOutput> output;
          const auto output_hr = adapter->EnumOutputs(output_index, &output);
          if (output_hr == DXGI_ERROR_NOT_FOUND) break;
          if (FAILED(output_hr)) { last_error = output_hr; continue; }
          DXGI_OUTPUT_DESC output_desc{};
          if (FAILED(output->GetDesc(&output_desc)) || !output_desc.AttachedToDesktop) continue;
          const bool is_target = output_desc.Monitor == target_monitor;
          if ((pass == 0) != is_target) continue;

          ComPtr<ID3D11Device> candidate_device;
          ComPtr<ID3D11DeviceContext> candidate_context;
          D3D_FEATURE_LEVEL level{};
          const auto device_hr = D3D11CreateDevice(
              adapter.Get(), D3D_DRIVER_TYPE_UNKNOWN, nullptr,
              D3D11_CREATE_DEVICE_BGRA_SUPPORT, nullptr, 0, D3D11_SDK_VERSION,
              &candidate_device, &level, &candidate_context);
          if (FAILED(device_hr)) { last_error = device_hr; continue; }
          ComPtr<IDXGIOutput1> output1;
          const auto output1_hr = output.As(&output1);
          if (FAILED(output1_hr)) { last_error = output1_hr; continue; }
          ComPtr<IDXGIOutputDuplication> candidate_duplication;
          const auto duplicate_hr =
              output1->DuplicateOutput(candidate_device.Get(), &candidate_duplication);
          if (FAILED(duplicate_hr)) { last_error = duplicate_hr; continue; }
          device_ = std::move(candidate_device);
          context_ = std::move(candidate_context);
          duplication_ = std::move(candidate_duplication);
          monitor_ = output_desc.Monitor;
          std::cerr << "Capturing desktop-pet monitor: "
                    << output_desc.DesktopCoordinates.right -
                           output_desc.DesktopCoordinates.left
                    << 'x'
                    << output_desc.DesktopCoordinates.bottom -
                           output_desc.DesktopCoordinates.top
                    << '\n';
          return;
        }
      }
    }
    std::cerr << hresult_message("DXGI desktop duplication unavailable; using GDI fallback", last_error) << '\n';
    start_gdi();
  }
  void stop() noexcept override {
    if (memory_dc_ && old_bitmap_) SelectObject(memory_dc_, old_bitmap_);
    if (bitmap_) DeleteObject(bitmap_);
    if (memory_dc_) DeleteDC(memory_dc_);
    if (screen_dc_) ReleaseDC(nullptr, screen_dc_);
    screen_dc_ = nullptr; memory_dc_ = nullptr; bitmap_ = nullptr; old_bitmap_ = nullptr; dib_bits_ = nullptr;
    gdi_fallback_ = false;
    monitor_ = nullptr;
    staging_.Reset(); duplication_.Reset(); context_.Reset(); device_.Reset();
  }
  std::optional<VideoFrame> next_frame(std::uint32_t timeout_ms) override {
    if (monitor_ && pet_monitor() != monitor_) {
      stop();
      start();
      return std::nullopt;
    }
    if (gdi_fallback_) return next_gdi_frame();
    if (!duplication_) throw std::runtime_error("desktop capture not started");
    DXGI_OUTDUPL_FRAME_INFO info{}; ComPtr<IDXGIResource> resource;
    const auto hr = duplication_->AcquireNextFrame(timeout_ms, &info, &resource);
    if (hr == DXGI_ERROR_WAIT_TIMEOUT) return std::nullopt;
    if (hr == DXGI_ERROR_ACCESS_LOST) { stop(); start(); return std::nullopt; }
    check(hr, "AcquireNextFrame failed");
    struct Release { IDXGIOutputDuplication* d; ~Release() { d->ReleaseFrame(); } } release{duplication_.Get()};
    ComPtr<ID3D11Texture2D> texture; check(resource.As(&texture), "frame texture unavailable");
    D3D11_TEXTURE2D_DESC desc{}; texture->GetDesc(&desc);
    if (!staging_ || desc.Width != width_ || desc.Height != height_) {
      desc.BindFlags = 0; desc.MiscFlags = 0; desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ; desc.Usage = D3D11_USAGE_STAGING;
      staging_.Reset(); check(device_->CreateTexture2D(&desc, nullptr, &staging_), "staging texture failed");
      width_ = desc.Width; height_ = desc.Height;
    }
    context_->CopyResource(staging_.Get(), texture.Get());
    D3D11_MAPPED_SUBRESOURCE mapped{}; check(context_->Map(staging_.Get(), 0, D3D11_MAP_READ, 0, &mapped), "Map failed");
    struct Unmap { ID3D11DeviceContext* c; ID3D11Resource* r; ~Unmap() { c->Unmap(r, 0); } } unmap{context_.Get(), staging_.Get()};
    VideoFrame frame; frame.width = width_; frame.height = height_; frame.row_pitch = width_ * 4U;
    if (info.LastPresentTime.QuadPart > 0) {
      LARGE_INTEGER frequency{}; QueryPerformanceFrequency(&frequency);
      const auto ticks = static_cast<std::uint64_t>(info.LastPresentTime.QuadPart);
      frame.timestamp_100ns = (ticks / static_cast<std::uint64_t>(frequency.QuadPart)) * 10'000'000ULL +
          (ticks % static_cast<std::uint64_t>(frequency.QuadPart)) * 10'000'000ULL /
              static_cast<std::uint64_t>(frequency.QuadPart);
    }
    frame.bgra.resize(std::size_t(frame.row_pitch) * frame.height);
    for (std::uint32_t y = 0; y < frame.height; ++y)
      std::memcpy(frame.bgra.data() + std::size_t(y) * frame.row_pitch,
                  static_cast<const std::byte*>(mapped.pData) + std::size_t(y) * mapped.RowPitch, frame.row_pitch);
    return frame;
  }
 private:
  void start_gdi() {
    monitor_ = pet_monitor();
    MONITORINFO monitor_info{.cbSize = sizeof(MONITORINFO)};
    if (!monitor_ || !GetMonitorInfoW(monitor_, &monitor_info)) {
      throw std::runtime_error("foreground monitor is unavailable");
    }
    left_ = monitor_info.rcMonitor.left;
    top_ = monitor_info.rcMonitor.top;
    width_ = static_cast<std::uint32_t>(
        monitor_info.rcMonitor.right - monitor_info.rcMonitor.left);
    height_ = static_cast<std::uint32_t>(
        monitor_info.rcMonitor.bottom - monitor_info.rcMonitor.top);
    if (!width_ || !height_) throw std::runtime_error("virtual desktop has invalid dimensions");
    screen_dc_ = GetDC(nullptr);
    memory_dc_ = screen_dc_ ? CreateCompatibleDC(screen_dc_) : nullptr;
    BITMAPINFO info{};
    info.bmiHeader.biSize = sizeof(BITMAPINFOHEADER);
    info.bmiHeader.biWidth = static_cast<LONG>(width_);
    info.bmiHeader.biHeight = -static_cast<LONG>(height_);
    info.bmiHeader.biPlanes = 1;
    info.bmiHeader.biBitCount = 32;
    info.bmiHeader.biCompression = BI_RGB;
    bitmap_ = memory_dc_ ? CreateDIBSection(memory_dc_, &info, DIB_RGB_COLORS, &dib_bits_, nullptr, 0) : nullptr;
    if (!screen_dc_ || !memory_dc_ || !bitmap_ || !dib_bits_) {
      stop();
      throw std::runtime_error("GDI desktop capture initialization failed");
    }
    old_bitmap_ = SelectObject(memory_dc_, bitmap_);
    gdi_fallback_ = true;
  }
  std::optional<VideoFrame> next_gdi_frame() {
    const auto now = std::chrono::steady_clock::now();
    if (now - last_gdi_frame_ < std::chrono::milliseconds(250)) return std::nullopt;
    last_gdi_frame_ = now;
    if (!BitBlt(memory_dc_, 0, 0, static_cast<int>(width_), static_cast<int>(height_),
                screen_dc_, left_, top_, SRCCOPY | CAPTUREBLT)) {
      throw std::runtime_error("GDI BitBlt failed");
    }
    VideoFrame frame;
    frame.width = width_; frame.height = height_; frame.row_pitch = width_ * 4U;
    frame.timestamp_100ns = static_cast<std::uint64_t>(
        std::chrono::duration_cast<std::chrono::nanoseconds>(now.time_since_epoch()).count() / 100);
    const auto bytes = std::size_t(frame.row_pitch) * frame.height;
    frame.bgra.resize(bytes);
    std::memcpy(frame.bgra.data(), dib_bits_, bytes);
    return frame;
  }
  ComPtr<ID3D11Device> device_; ComPtr<ID3D11DeviceContext> context_;
  ComPtr<IDXGIOutputDuplication> duplication_; ComPtr<ID3D11Texture2D> staging_;
  std::uint32_t width_{}, height_{};
  HDC screen_dc_{}; HDC memory_dc_{}; HBITMAP bitmap_{}; HGDIOBJ old_bitmap_{}; void* dib_bits_{};
  int left_{}, top_{};
  HMONITOR monitor_{};
  bool gdi_fallback_{};
  std::chrono::steady_clock::time_point last_gdi_frame_{};
};
} // namespace
std::unique_ptr<IDesktopCapture> make_dxgi_desktop_capture() { return std::make_unique<DxgiDesktopCapture>(); }
} // namespace jarvis::win
#endif
