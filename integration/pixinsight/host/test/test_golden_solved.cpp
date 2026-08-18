// Solved-mode byte-identity golden test (Task 8).
//
// Drives the real `mmm-ipc-worker` twice through the C++ `mmm::Host`:
//   1. Files(Solved) mode -- the worker reads solvedN.xisf itself, reprojects
//      each raw plate-solved panel onto the chosen mosaic frame, and blends;
//      the output is the golden reference.
//   2. Solved mode -- our in-memory PanelSource serves the SAME raw pixels
//      (solvedN.bin) over shared memory on the worker's BandRequests, with
//      each panel's plate solution spliced into its `properties` from
//      `solved_props.json`. `mmm::Host::probe_frame` sizes the output slots
//      up front, exactly as a real PixInsight host must.
// Then asserts the two blended mosaics are byte-identical -- proving both
// the Solved-mode serve loop AND probe-based slot sizing are correct. A
// wrong slot size (input panel width vs. reprojected frame width) would
// silently corrupt the shm bands and fail this compare.

#include <algorithm>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <fstream>
#include <string>
#include <vector>

#include "mmm_host.h"
#include "test/golden_harness.h"
#include "test/test_util.h"
#include "third_party/json.hpp"

namespace {

using nlohmann::json;
using mmm_test::BufCollector;
using mmm_test::make_params;
using mmm_test::MemSource;
using mmm_test::NeverSource;
using mmm_test::run_job;
using mmm_test::RunResult;

// Reads a whole file into a byte vector.
std::vector<uint8_t> read_bytes(const std::string& path) {
  std::ifstream f(path, std::ios::binary);
  CHECK(f.good());
  return std::vector<uint8_t>((std::istreambuf_iterator<char>(f)),
                              std::istreambuf_iterator<char>());
}

// Reads a raw native-endian planar f32 file (solvedN.bin) into a float vector.
std::vector<float> read_floats(const std::string& path, uint64_t count) {
  std::vector<uint8_t> bytes = read_bytes(path);
  CHECK(bytes.size() == count * sizeof(float));
  std::vector<float> out(count);
  std::memcpy(out.data(), bytes.data(), bytes.size());
  return out;
}

// Builds the `panels` array from solved_meta.json, optionally splicing each
// panel's `properties` array in (from solved_props.json). Files(Solved) mode
// needs no properties (the worker re-derives them from the files it reads
// itself); Solved mode needs the real plate solution here.
json make_panels(const json& meta, const json* props /* nullable */) {
  json panels = json::array();
  size_t i = 0;
  for (const auto& p : meta.at("panels")) {
    json pd;
    pd["panel_id"] = p.at("id").get<uint32_t>();
    pd["width"] = p.at("w").get<uint64_t>();
    pd["height"] = p.at("h").get<uint64_t>();
    pd["channels"] = p.at("ch").get<uint64_t>();
    pd["properties"] = (props != nullptr) ? (*props)[i] : json::array();
    panels.push_back(pd);
    i++;
  }
  return panels;
}

}  // namespace

int main(int argc, char** argv) {
  CHECK(argc >= 3);
  const std::string fixtures = argv[1];
  const std::string worker_path = argv[2];

  json meta = json::parse(read_bytes(fixtures + "/solved_meta.json"));
  json props = json::parse(read_bytes(fixtures + "/solved_props.json"));
  const uint64_t ch = meta.at("ch").get<uint64_t>();
  const uint32_t band_rows = meta.at("band_rows").get<uint32_t>();
  const double feather_px = meta.at("feather_px").get<double>();
  const size_t n_panels = meta.at("panels").size();
  CHECK(props.is_array());
  CHECK(props.size() == n_panels);

  const int pid = mmm_test_getpid();
  json params = make_params(band_rows, feather_px);

  // Max raw panel width, for slot sizing (an input band is one raw panel's
  // own width; see the frame-probe below for the output side).
  uint64_t max_panel_w = 0;
  for (const auto& p : meta.at("panels")) {
    max_panel_w = std::max<uint64_t>(max_panel_w, p.at("w").get<uint64_t>());
  }

  // ---- Probe the reprojected frame's width so shm slots fit BOTH an input
  // band (one raw panel's width) and an output band (the reprojected mosaic
  // frame's width, which can exceed any single panel's width). This is the
  // same `probe_frame` a real PixInsight host calls before creating its shm
  // segment; a wrong choice here (e.g. using only max_panel_w) would corrupt
  // whichever side is larger and fail the byte-identity compare below. ----
  json probe_init;
  // protocol_version + worker_version are stamped by Host (run/probe) itself.
  probe_init["shm_name"] = "";
  probe_init["slot_bytes"] = 0;
  probe_init["input_slots"] = 0;
  probe_init["output_slots"] = 0;
  probe_init["canvas"] = {0, 0, ch};
  probe_init["panels"] = make_panels(meta, &props);
  probe_init["mode"] = "Solved";
  probe_init["session_dir"] = "";
  probe_init["params"] = params;

  uint64_t frame_w = 0, frame_h = 0, frame_ch = 0;
  mmm::Host::probe_frame(worker_path, probe_init, frame_w, frame_h, frame_ch);
  CHECK(frame_ch == ch);
  CHECK(frame_w > 0 && frame_h > 0);

  const uint64_t slot_bytes = std::max<uint64_t>(max_panel_w, frame_w) * ch * band_rows * 4;
  mmm::SlotLayout layout{slot_bytes, 8, 2};

  // ---- Run 1: Files(Solved)-mode golden. Worker reads the .xisf files
  // itself and re-derives the plate solution from their headers, so this
  // run's `panels[].properties` can stay empty. ----
  json files_paths = json::array();
  for (size_t i = 0; i < n_panels; i++) {
    files_paths.push_back(fixtures + "/solved" + std::to_string(i) + ".xisf");
  }
  json files_init;
  // protocol_version + worker_version are stamped by Host (run/probe) itself.
  files_init["shm_name"] = "";  // filled below
  files_init["slot_bytes"] = slot_bytes;
  files_init["input_slots"] = layout.input_slots;
  files_init["output_slots"] = layout.output_slots;
  files_init["canvas"] = {0, 0, ch};  // unused by Files(Solved); worker derives its own.
  files_init["panels"] = make_panels(meta, nullptr);
  files_init["mode"] =
      json{{"Files", {{"paths", files_paths}, {"input_select", "Solved"}}}};
  files_init["session_dir"] = fixtures + "/solved_files_" + std::to_string(pid) + ".mmm-session";
  files_init["params"] = params;

  const std::string files_shm = "/mmm-gsolved-files-" + std::to_string(pid);
  files_init["shm_name"] = files_shm;
  NeverSource never;
  RunResult golden_run = run_job(worker_path, files_init, layout, files_shm, never);
  const std::vector<float>& golden = golden_run.data;

  // ---- Run 2: Solved-mode. Our MemSource serves solvedN.bin over shm, with
  // each panel's plate solution spliced into `properties`. ----
  MemSource mem;
  for (size_t i = 0; i < n_panels; i++) {
    const auto& pj = meta.at("panels")[i];
    uint64_t pw = pj.at("w"), ph = pj.at("h"), pc = pj.at("ch");
    mem.w.push_back(pw);
    mem.h.push_back(ph);
    mem.ch.push_back(pc);
    mem.panels.push_back(
        read_floats(fixtures + "/solved" + std::to_string(i) + ".bin", pw * ph * pc));
  }

  json solved_init;
  // protocol_version + worker_version are stamped by Host (run/probe) itself.
  solved_init["shm_name"] = "";  // filled below
  solved_init["slot_bytes"] = slot_bytes;
  solved_init["input_slots"] = layout.input_slots;
  solved_init["output_slots"] = layout.output_slots;
  solved_init["canvas"] = {0, 0, ch};  // unused by Solved mode; worker derives its own frame.
  solved_init["panels"] = make_panels(meta, &props);
  solved_init["mode"] = "Solved";
  solved_init["session_dir"] = fixtures + "/solved_shm_" + std::to_string(pid) + ".mmm-session";
  solved_init["params"] = params;

  const std::string solved_shm = "/mmm-golden-solved-shm-" + std::to_string(pid);
  solved_init["shm_name"] = solved_shm;
  RunResult got_run = run_job(worker_path, solved_init, layout, solved_shm, mem);
  const std::vector<float>& got = got_run.data;

  // ---- Assertions. ----
  // The output canvas is the blend's data bounding box within the reprojected
  // frame (worker-computed) -- generally smaller than the full `choose_frame`
  // frame `probe_frame` reported (that frame sizes the reprojection/blend
  // canvas and hence the shm slots; the final streamed mosaic then crops to
  // wherever panel data actually landed). Both runs must still agree on the
  // bounding box with each other.
  CHECK(golden_run.w == got_run.w);
  CHECK(golden_run.h == got_run.h);
  CHECK(golden_run.ch == got_run.ch);
  CHECK(got_run.w <= frame_w);
  CHECK(got_run.h <= frame_h);
  CHECK(!golden.empty());
  CHECK(golden.size() == golden_run.w * golden_run.h * golden_run.ch);
  // Non-vacuous: golden must be non-constant (not all-zero, not flat).
  bool any_nonzero = false;
  float lo = golden[0], hi = golden[0];
  for (float v : golden) {
    if (v != 0.0f) any_nonzero = true;
    if (v < lo) lo = v;
    if (v > hi) hi = v;
  }
  CHECK(any_nonzero);
  CHECK(hi - lo > 1e-6f);

  // Byte-identity: solved shm output == files(solved)-mode golden, element
  // for element. An undersized slot (probe width < panel width, or vice
  // versa) would silently truncate/interleave bands here and fail this loop.
  CHECK(got.size() == golden.size());
  size_t first_mismatch = got.size();
  for (size_t i = 0; i < got.size(); i++) {
    if (got[i] != golden[i]) {
      first_mismatch = i;
      break;
    }
  }
  if (first_mismatch != got.size()) {
    std::fprintf(stderr, "byte-identity FAILED: first mismatch at element %zu: golden=%.9g got=%.9g\n",
                 first_mismatch, static_cast<double>(golden[first_mismatch]),
                 static_cast<double>(got[first_mismatch]));
  }
  CHECK(first_mismatch == got.size());

  std::printf(
      "test_golden_solved OK: %zu elements byte-identical (range [%.6g, %.6g]), "
      "probed frame %llu x %llu x %llu, slot_bytes=%llu\n",
      got.size(), static_cast<double>(lo), static_cast<double>(hi),
      static_cast<unsigned long long>(frame_w), static_cast<unsigned long long>(frame_h),
      static_cast<unsigned long long>(frame_ch), static_cast<unsigned long long>(slot_bytes));
  return 0;
}
