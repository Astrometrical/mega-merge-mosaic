// rogue_worker.cpp -- a scripted stand-in worker for test_validation.
//
// Speaks just enough of the wire protocol (PROTOCOL.md Sections 3-6) to send
// one scenario's worth of frames to its stdout, then blocks draining stdin
// until EOF (so the host can deliver BandReply/OutputAck frames without
// hitting a closed pipe, and reaps this process the moment it closes our
// stdin -- or kills us after a validation failure).
//
// Each scenario models a MISBEHAVING worker -- out-of-range band geometry,
// bad slot ids, duplicate/absent Begin -- that `mmm::Host::run()` must
// refuse with a clean HostError instead of corrupting host memory. The
// "valid" scenario proves the framing here is right, so a failing attack
// scenario indicts the host's validation, not this binary's encoding.
//
// Never reads Init (it sits in the pipe buffer); the scripted geometry is
// agreed with test_validation.cpp per scenario.

#include <cstdint>
#include <cstdio>
#include <cstring>
#include <string>
#include <vector>

#ifdef _WIN32
#include <fcntl.h>
#include <io.h>
#endif

namespace {

void put_u32(std::vector<uint8_t>& b, uint32_t v) {
  for (int i = 0; i < 4; i++) b.push_back(static_cast<uint8_t>(v >> (8 * i)));
}

void put_u64(std::vector<uint8_t>& b, uint64_t v) {
  for (int i = 0; i < 8; i++) b.push_back(static_cast<uint8_t>(v >> (8 * i)));
}

void write_frame(uint8_t tag, const std::vector<uint8_t>& payload) {
  std::vector<uint8_t> f;
  f.push_back(tag);
  put_u32(f, static_cast<uint32_t>(payload.size()));
  f.insert(f.end(), payload.begin(), payload.end());
  std::fwrite(f.data(), 1, f.size(), stdout);
  std::fflush(stdout);
}

void send_begin(uint64_t w, uint64_t h, uint64_t ch) {
  std::string j = "{\"Begin\":{\"w\":" + std::to_string(w) + ",\"h\":" + std::to_string(h) +
                  ",\"ch\":" + std::to_string(ch) + "}}";
  write_frame(3, std::vector<uint8_t>(j.begin(), j.end()));
}

void send_output_band(uint32_t request_id, uint64_t y0, uint64_t rows, uint32_t slot_id) {
  std::vector<uint8_t> p;
  put_u32(p, request_id);
  put_u64(p, y0);
  put_u64(p, rows);
  put_u32(p, slot_id);
  write_frame(4, p);
}

void send_band_request(uint32_t request_id, uint32_t panel_id, uint64_t y0, uint64_t y1,
                       uint32_t slot_id) {
  std::vector<uint8_t> p;
  put_u32(p, request_id);
  put_u32(p, panel_id);
  put_u64(p, y0);
  put_u64(p, y1);
  put_u32(p, slot_id);
  write_frame(1, p);
}

void send_done() {
  std::string j = "\"Done\"";
  write_frame(5, std::vector<uint8_t>(j.begin(), j.end()));
}

// Block until the host closes our stdin (after Done on the success path) or
// kills us (after a validation failure).
void drain_stdin_until_eof() {
  char buf[4096];
  while (std::fread(buf, 1, sizeof buf, stdin) > 0) {
  }
}

}  // namespace

int main(int argc, char** argv) {
#ifdef _WIN32
  _setmode(_fileno(stdout), _O_BINARY);
  _setmode(_fileno(stdin), _O_BINARY);
#endif
  if (argc < 2) {
    std::fprintf(stderr, "rogue_worker: missing scenario argument\n");
    return 2;
  }
  const std::string scenario = argv[1];

  if (scenario == "valid") {
    // Canvas 32x32x1; one in-range 8-row band, then Done.
    send_begin(32, 32, 1);
    send_output_band(1, 0, 8, 0);
    send_done();
  } else if (scenario == "band_before_begin") {
    send_output_band(1, 0, 8, 0);
  } else if (scenario == "band_past_end") {
    // y0 + rows = 36 > h = 32: would memcpy past the image's channel plane.
    send_begin(32, 32, 1);
    send_output_band(1, 28, 8, 0);
  } else if (scenario == "band_slot_oob") {
    // Output slot 9 with output_slots = 2: would read outside the shm mapping.
    send_begin(32, 32, 1);
    send_output_band(1, 0, 8, 9);
  } else if (scenario == "band_overflows_slot") {
    // Canvas 32x1024x1 (init agrees); 64 rows fit the canvas but 64*32*4
    // bytes overrun the 8-row slot the layout sized.
    send_begin(32, 1024, 1);
    send_output_band(1, 0, 64, 0);
  } else if (scenario == "begin_twice") {
    send_begin(32, 32, 1);
    send_begin(32, 32, 1);
  } else if (scenario == "begin_mismatch") {
    // Init canvas says 32x32x1.
    send_begin(64, 64, 1);
  } else if (scenario == "begin_zero") {
    // Init canvas [0,0,1] (solved-style, w/h unknown): zero dims still refused.
    send_begin(0, 0, 1);
  } else if (scenario == "begin_huge") {
    // Init canvas [0,0,1]: a width beyond int32 would truncate in the
    // collector's ImageWindow construction.
    send_begin(uint64_t(1) << 32, 8, 1);
  } else if (scenario == "request_slot_oob") {
    // Input slot 9 with input_slots = 4: the fill would land outside the shm.
    send_band_request(1, 0, 0, 8, 9);
  } else if (scenario == "request_overflows_slot") {
    // Panel is 32x32x1 and the request is in panel range, but 32 rows *
    // 32 px * 4 bytes overrun the 8-row input slot: the host must refuse
    // BEFORE asking its PanelSource to fill.
    send_band_request(1, 0, 0, 32, 0);
  } else {
    std::fprintf(stderr, "rogue_worker: unknown scenario %s\n", scenario.c_str());
    return 2;
  }

  drain_stdin_until_eof();
  return 0;
}
