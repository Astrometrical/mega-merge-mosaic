#pragma once
// Wire codec for the host<->worker control channel, mirroring
// crates/mmm-core/src/ipc/protocol.rs. Frame format:
//   [u8 tag][u32 LE payload_len][payload bytes]
// Band-carrying messages (BandRequest/BandReply/OutputBand) are fixed
// little-endian binary payloads; everything else is JSON (nlohmann).
// See integration/pixinsight/PROTOCOL.md Sections 3-6 for the full spec.
// No PCL headers here -- only the C++ stdlib, POSIX, and vendored json.hpp.

#include <cstdint>
#include <string>
#include <vector>

#include "third_party/json.hpp"

namespace mmm {

/// Tags for frames the worker writes to its stdout (worker -> host). `Invalid`
/// is never sent on the wire (real worker tags are 1-6); it exists only as a
/// safe default for a default-constructed `WorkerFrame` so an unfilled frame
/// can never be mistaken for a real message (see `WorkerFrame::tag`).
enum class WorkerTag : uint8_t {
  Invalid = 0,
  BandRequest = 1,
  Progress = 2,
  Begin = 3,
  OutputBand = 4,
  Done = 5,
  Error = 6,
};

/// Tags for frames the host writes to the worker's stdin (host -> worker).
enum class HostTag : uint8_t {
  Init = 128,
  BandReply = 129,
  OutputAck = 130,
  Cancel = 131,
};

/// Worker -> host, tag 1, 28 bytes. Asks the host to fill a shared-memory
/// input slot with rows [y0, y1) of panel `panel_id`. Field order/offsets
/// match PROTOCOL.md Section 5 exactly.
struct BandRequest {
  uint32_t request_id;
  uint32_t panel_id;
  uint64_t y0;
  uint64_t y1;
  uint32_t slot_id;
};

/// Host -> worker, tag 129, 9 bytes. Tells the worker the band it asked
/// for is ready in `slot_id` (or that filling it failed).
struct BandReply {
  uint32_t request_id;
  uint32_t slot_id;
  uint8_t status;  // 0 = ok, 1 = error
};

/// Worker -> host, tag 4, 24 bytes. Tells the host a blended output band
/// is ready in output slot `slot_id`.
struct OutputBand {
  uint32_t request_id;
  uint64_t y0;
  uint64_t rows;
  uint32_t slot_id;
};

/// A decoded worker->host frame (binary or JSON), tagged union style: only
/// the fields matching `tag` are meaningful.
struct WorkerFrame {
  WorkerTag tag = WorkerTag::Invalid;

  BandRequest band_request{};  // valid iff tag == BandRequest
  OutputBand output_band{};    // valid iff tag == OutputBand

  // Progress (tag 2)
  std::string progress_stage;
  uint64_t progress_done = 0;
  uint64_t progress_total = 0;

  // Begin (tag 3)
  uint64_t begin_w = 0;
  uint64_t begin_h = 0;
  uint64_t begin_ch = 0;

  // Error (tag 6)
  std::string error_message;
};

/// Reads one worker->host frame from `fd` (blocking). Returns false on a
/// clean EOF encountered before any byte of a new frame (the worker exited
/// cleanly). Throws `std::runtime_error` if EOF is hit mid-length-prefix or
/// mid-payload (a truncated frame), or if the tag is not one of the six
/// worker tags.
bool read_worker_frame(int fd, WorkerFrame& out);

/// Writes one frame: tag byte, 4-byte LE payload length, then `len` bytes
/// of payload. The caller is responsible for serializing writes under a
/// mutex when called from multiple threads; this function does not lock.
void write_frame_raw(int fd, uint8_t tag, const uint8_t* payload, uint32_t len);

/// Encodes a `BandReply` as its 9-byte little-endian wire form (does not
/// frame it -- pass the result to `write_frame_raw` with tag 129).
std::vector<uint8_t> encode_band_reply(const BandReply& r);

/// Builds a framed `OutputAck { request_id }` frame (tag 130, JSON
/// `{"OutputAck":{"request_id":N}}`), ready to write with a single `write`.
std::vector<uint8_t> encode_output_ack(uint32_t request_id);

/// Builds a framed `Cancel` frame (tag 131, JSON bare string `"Cancel"`),
/// ready to write with a single `write`.
std::vector<uint8_t> encode_cancel();

/// Builds a framed `Init` frame (tag 128) from a pre-built nlohmann JSON
/// value (the caller constructs `{"Init": {...InitJob shape...}}` per
/// PROTOCOL.md Section 6). Recursively walks `init_obj` first and throws
/// `std::runtime_error` if any `is_number_float()` node holds a non-finite
/// value (NaN/Infinity have no JSON representation -- mirrors Rust
/// `write_frame`'s `validate_finite` guard). Returns the fully framed
/// bytes (tag + length + payload), ready to write with a single `write`.
std::vector<uint8_t> encode_init(const nlohmann::json& init_obj);

}  // namespace mmm
