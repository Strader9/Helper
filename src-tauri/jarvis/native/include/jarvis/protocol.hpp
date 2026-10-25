#pragma once

#include <cstddef>
#include <cstdint>
#include <span>
#include <string>
#include <vector>

namespace jarvis::ipc {

inline constexpr std::uint32_t kMagic = 0x56524A41U; // "AJRV" on the wire
inline constexpr std::uint16_t kProtocolVersion = 1;
inline constexpr std::size_t kHeaderBytes = 32;
inline constexpr std::uint32_t kMaxPayloadBytes = 16U * 1024U * 1024U;

enum class MessageType : std::uint16_t {
  hello = 1,
  start = 2,
  stop = 3,
  submit = 4,
  cancel = 5,
  result = 6,
  status = 7,
  error = 8,
  shutdown = 9,
  configure_game = 10,
  start_duplex = 11,
  stop_duplex = 12,
};

enum class StatusCode : std::uint32_t {
  ok = 0,
  malformed = 1,
  unsupported_version = 2,
  unavailable = 3,
  cancelled = 4,
  internal_error = 5,
};

struct FrameHeader {
  std::uint32_t magic{kMagic};
  std::uint16_t version{kProtocolVersion};
  MessageType type{MessageType::status};
  std::uint32_t flags{};
  std::uint64_t request_id{};
  std::uint32_t payload_size{};
  std::uint32_t payload_crc32{};
  std::uint32_t reserved{};
};

struct Message {
  FrameHeader header{};
  std::vector<std::byte> payload{};
};

struct DecodeResult {
  Message message{};
  std::size_t consumed{};
  std::string error{};
  [[nodiscard]] explicit operator bool() const noexcept { return error.empty(); }
};

[[nodiscard]] std::uint32_t crc32(std::span<const std::byte> bytes) noexcept;
[[nodiscard]] std::vector<std::byte> encode(MessageType type, std::uint64_t request_id,
                                             std::span<const std::byte> payload,
                                             std::uint32_t flags = 0);
[[nodiscard]] DecodeResult decode(std::span<const std::byte> bytes);

} // namespace jarvis::ipc
