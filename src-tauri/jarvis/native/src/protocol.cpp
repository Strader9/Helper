#include "jarvis/protocol.hpp"

#include <array>
#include <cstring>
#include <type_traits>

namespace jarvis::ipc {
namespace {
template <class T> void append_le(std::vector<std::byte>& out, T value) {
  using U = std::make_unsigned_t<T>;
  U bits = static_cast<U>(value);
  for (std::size_t i = 0; i < sizeof(T); ++i) out.push_back(std::byte((bits >> (i * 8U)) & 0xffU));
}
template <class T> T read_le(std::span<const std::byte> bytes, std::size_t& at) {
  using U = std::make_unsigned_t<T>;
  U value{};
  for (std::size_t i = 0; i < sizeof(T); ++i) value |= U(std::to_integer<unsigned>(bytes[at++])) << (i * 8U);
  return static_cast<T>(value);
}
} // namespace

std::uint32_t crc32(std::span<const std::byte> bytes) noexcept {
  std::uint32_t crc = 0xffffffffU;
  for (const auto byte : bytes) {
    crc ^= std::to_integer<std::uint8_t>(byte);
    for (int bit = 0; bit < 8; ++bit) crc = (crc >> 1U) ^ (0xedb88320U & (0U - (crc & 1U)));
  }
  return ~crc;
}

std::vector<std::byte> encode(MessageType type, std::uint64_t request_id,
                              std::span<const std::byte> payload, std::uint32_t flags) {
  if (payload.size() > kMaxPayloadBytes) return {};
  std::vector<std::byte> out;
  out.reserve(kHeaderBytes + payload.size());
  append_le(out, kMagic); append_le(out, kProtocolVersion); append_le(out, static_cast<std::uint16_t>(type));
  append_le(out, flags); append_le(out, request_id); append_le(out, static_cast<std::uint32_t>(payload.size()));
  append_le(out, crc32(payload)); append_le(out, std::uint32_t{});
  out.insert(out.end(), payload.begin(), payload.end());
  return out;
}

DecodeResult decode(std::span<const std::byte> bytes) {
  DecodeResult result;
  if (bytes.size() < kHeaderBytes) { result.error = "incomplete header"; return result; }
  std::size_t at{};
  auto& h = result.message.header;
  h.magic = read_le<std::uint32_t>(bytes, at);
  h.version = read_le<std::uint16_t>(bytes, at);
  h.type = static_cast<MessageType>(read_le<std::uint16_t>(bytes, at));
  h.flags = read_le<std::uint32_t>(bytes, at); h.request_id = read_le<std::uint64_t>(bytes, at);
  h.payload_size = read_le<std::uint32_t>(bytes, at); h.payload_crc32 = read_le<std::uint32_t>(bytes, at);
  h.reserved = read_le<std::uint32_t>(bytes, at);
  if (h.magic != kMagic) { result.error = "invalid magic"; return result; }
  if (h.version != kProtocolVersion) { result.error = "unsupported protocol version"; return result; }
  if (h.payload_size > kMaxPayloadBytes) { result.error = "payload too large"; return result; }
  if (bytes.size() < kHeaderBytes + h.payload_size) { result.error = "incomplete payload"; return result; }
  const auto payload = bytes.subspan(kHeaderBytes, h.payload_size);
  if (crc32(payload) != h.payload_crc32) { result.error = "payload checksum mismatch"; return result; }
  result.message.payload.assign(payload.begin(), payload.end());
  result.consumed = kHeaderBytes + h.payload_size;
  return result;
}
} // namespace jarvis::ipc
