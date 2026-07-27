#include "mmm_protocol.h"

#include <cerrno>
#include <cmath>
#include <cstring>
#include <stdexcept>
#include <unistd.h>

namespace mmm {

namespace {

// ---- little-endian helpers -------------------------------------------

uint32_t get_u32_le(const uint8_t* buf, size_t at) {
  return static_cast<uint32_t>(buf[at]) | (static_cast<uint32_t>(buf[at + 1]) << 8) |
         (static_cast<uint32_t>(buf[at + 2]) << 16) | (static_cast<uint32_t>(buf[at + 3]) << 24);
}

uint64_t get_u64_le(const uint8_t* buf, size_t at) {
  uint64_t v = 0;
  for (int i = 7; i >= 0; --i) {
    v = (v << 8) | buf[at + static_cast<size_t>(i)];
  }
  return v;
}

void put_u32_le(std::vector<uint8_t>& buf, uint32_t v) {
  for (int i = 0; i < 4; ++i) {
    buf.push_back(static_cast<uint8_t>((v >> (8 * i)) & 0xff));
  }
}

// ---- full read/write over a raw fd (handle short reads/writes + EINTR) --

// Reads up to `n` bytes into `buf`, looping over `read()` until `n` bytes
// have been read or EOF is hit. Returns the number of bytes actually read
// (< n only on EOF). Throws on a hard read error.
size_t full_read(int fd, uint8_t* buf, size_t n) {
  size_t total = 0;
  while (total < n) {
    ssize_t r = ::read(fd, buf + total, n - total);
    if (r < 0) {
      if (errno == EINTR) continue;
      throw std::runtime_error(std::string("read: ") + std::strerror(errno));
    }
    if (r == 0) break;  // EOF
    total += static_cast<size_t>(r);
  }
  return total;
}

void full_write(int fd, const uint8_t* buf, size_t n) {
  size_t total = 0;
  while (total < n) {
    ssize_t w = ::write(fd, buf + total, n - total);
    if (w < 0) {
      if (errno == EINTR) continue;
      throw std::runtime_error(std::string("write: ") + std::strerror(errno));
    }
    total += static_cast<size_t>(w);
  }
}

std::vector<uint8_t> build_frame(uint8_t tag, const std::string& payload) {
  std::vector<uint8_t> buf;
  buf.reserve(5 + payload.size());
  buf.push_back(tag);
  put_u32_le(buf, static_cast<uint32_t>(payload.size()));
  buf.insert(buf.end(), payload.begin(), payload.end());
  return buf;
}

// Recursively checks every `is_number_float()` node in `node` is finite;
// throws otherwise. Mirrors Rust `write_frame`'s `validate_finite` guard
// (PROTOCOL.md Section 6's finite-float precondition) -- JSON cannot
// represent NaN/Infinity.
void check_finite(const nlohmann::json& node) {
  if (node.is_number_float()) {
    if (!std::isfinite(node.get<double>())) {
      throw std::runtime_error("Init payload contains a non-finite float (NaN/Infinity)");
    }
  } else if (node.is_object()) {
    for (auto it = node.begin(); it != node.end(); ++it) {
      check_finite(it.value());
    }
  } else if (node.is_array()) {
    for (const auto& el : node) {
      check_finite(el);
    }
  }
}

}  // namespace

bool read_worker_frame(int fd, WorkerFrame& out) {
  uint8_t tag_byte = 0;
  size_t n = full_read(fd, &tag_byte, 1);
  if (n == 0) {
    return false;  // clean EOF before any byte of a new frame
  }

  uint8_t len_buf[4];
  if (full_read(fd, len_buf, 4) != 4) {
    throw std::runtime_error("read_worker_frame: truncated frame (EOF mid-length-prefix)");
  }
  uint32_t len = get_u32_le(len_buf, 0);

  std::vector<uint8_t> payload(len);
  if (len > 0 && full_read(fd, payload.data(), len) != len) {
    throw std::runtime_error("read_worker_frame: truncated frame (EOF mid-payload)");
  }

  switch (tag_byte) {
    case static_cast<uint8_t>(WorkerTag::BandRequest): {
      if (payload.size() != 28) {
        throw std::runtime_error("read_worker_frame: BandRequest expected 28 bytes, got " +
                                  std::to_string(payload.size()));
      }
      out.tag = WorkerTag::BandRequest;
      out.band_request.request_id = get_u32_le(payload.data(), 0);
      out.band_request.panel_id = get_u32_le(payload.data(), 4);
      out.band_request.y0 = get_u64_le(payload.data(), 8);
      out.band_request.y1 = get_u64_le(payload.data(), 16);
      out.band_request.slot_id = get_u32_le(payload.data(), 24);
      break;
    }
    case static_cast<uint8_t>(WorkerTag::OutputBand): {
      if (payload.size() != 24) {
        throw std::runtime_error("read_worker_frame: OutputBand expected 24 bytes, got " +
                                  std::to_string(payload.size()));
      }
      out.tag = WorkerTag::OutputBand;
      out.output_band.request_id = get_u32_le(payload.data(), 0);
      out.output_band.y0 = get_u64_le(payload.data(), 4);
      out.output_band.rows = get_u64_le(payload.data(), 12);
      out.output_band.slot_id = get_u32_le(payload.data(), 20);
      break;
    }
    case static_cast<uint8_t>(WorkerTag::Progress): {
      auto j = nlohmann::json::parse(payload.begin(), payload.end());
      const auto& p = j.at("Progress");
      out.tag = WorkerTag::Progress;
      out.progress_stage = p.at("stage").get<std::string>();
      out.progress_done = p.at("done").get<uint64_t>();
      out.progress_total = p.at("total").get<uint64_t>();
      break;
    }
    case static_cast<uint8_t>(WorkerTag::Begin): {
      auto j = nlohmann::json::parse(payload.begin(), payload.end());
      const auto& b = j.at("Begin");
      out.tag = WorkerTag::Begin;
      out.begin_w = b.at("w").get<uint64_t>();
      out.begin_h = b.at("h").get<uint64_t>();
      out.begin_ch = b.at("ch").get<uint64_t>();
      break;
    }
    case static_cast<uint8_t>(WorkerTag::Done): {
      // Bare JSON string "Done" -- parse to validate shape, no fields to keep.
      const auto j = nlohmann::json::parse(payload.begin(), payload.end());
      (void)j;
      out.tag = WorkerTag::Done;
      break;
    }
    case static_cast<uint8_t>(WorkerTag::Error): {
      auto j = nlohmann::json::parse(payload.begin(), payload.end());
      const auto& e = j.at("Error");
      out.tag = WorkerTag::Error;
      out.error_message = e.at("message").get<std::string>();
      break;
    }
    default:
      throw std::runtime_error("read_worker_frame: unknown tag " + std::to_string(tag_byte));
  }
  return true;
}

void write_frame_raw(int fd, uint8_t tag, const uint8_t* payload, uint32_t len) {
  std::vector<uint8_t> header;
  header.reserve(5);
  header.push_back(tag);
  put_u32_le(header, len);
  full_write(fd, header.data(), header.size());
  if (len > 0) {
    full_write(fd, payload, len);
  }
}

std::vector<uint8_t> encode_band_reply(const BandReply& r) {
  std::vector<uint8_t> buf;
  buf.reserve(9);
  put_u32_le(buf, r.request_id);
  put_u32_le(buf, r.slot_id);
  buf.push_back(r.status);
  return buf;
}

std::vector<uint8_t> encode_output_ack(uint32_t request_id) {
  nlohmann::json j;
  j["OutputAck"]["request_id"] = request_id;
  return build_frame(static_cast<uint8_t>(HostTag::OutputAck), j.dump());
}

std::vector<uint8_t> encode_cancel() {
  nlohmann::json j = "Cancel";
  return build_frame(static_cast<uint8_t>(HostTag::Cancel), j.dump());
}

std::vector<uint8_t> encode_init(const nlohmann::json& init_obj) {
  check_finite(init_obj);
  return build_frame(static_cast<uint8_t>(HostTag::Init), init_obj.dump());
}

}  // namespace mmm
