// Aligned byte-identity golden test (Task 7).
//
// Drives the real `mmm-ipc-worker` twice through the C++ `mmm::Host`:
//   1. Files mode  -- the worker reads panelN.xisf itself and blends; the
//      output is the golden reference.
//   2. Aligned mode -- our in-memory PanelSource serves the SAME pixels
//      (panelN.bin) over shared memory on the worker's BandRequests.
// Then asserts the two blended mosaics are byte-identical -- proving the Host
// serve loop reproduces the reference host (testhost.rs) exactly. Guards
// against a vacuous pass by requiring the golden to be non-empty and
// non-constant.

#include <unistd.h>

#include <cstdint>
#include <cstdio>
#include <cstring>
#include <fstream>
#include <string>
#include <vector>

#include "mmm_host.h"
#include "test/test_util.h"
#include "third_party/json.hpp"

namespace {

using nlohmann::json;

// Reads a whole file into a byte vector.
std::vector<uint8_t> read_bytes(const std::string& path) {
  std::ifstream f(path, std::ios::binary);
  CHECK(f.good());
  return std::vector<uint8_t>((std::istreambuf_iterator<char>(f)),
                              std::istreambuf_iterator<char>());
}

// Reads a raw native-endian planar f32 file (panelN.bin) into a float vector.
std::vector<float> read_floats(const std::string& path, uint64_t count) {
  std::vector<uint8_t> bytes = read_bytes(path);
  CHECK(bytes.size() == count * sizeof(float));
  std::vector<float> out(count);
  std::memcpy(out.data(), bytes.data(), bytes.size());
  return out;
}

// Serves band requests from in-memory planar panels (panelN.bin). Planar
// layout channels x height x width; fill_band copies rows [y0,y1) into the
// slot in the band's planar layout: index(c,r,x) = c*rows*w + r*w + x.
struct MemSource : mmm::PanelSource {
  std::vector<std::vector<float>> panels;
  std::vector<uint64_t> w, h, ch;
  bool fill_band(uint32_t id, uint64_t y0, uint64_t y1, float* dst) override {
    const auto& p = panels[id];
    uint64_t W = w[id], H = h[id], C = ch[id], rows = y1 - y0;
    for (uint64_t c = 0; c < C; c++) {
      for (uint64_t r = 0; r < rows; r++) {
        std::memcpy(dst + (c * rows + r) * W, &p[(c * H + y0 + r) * W], W * sizeof(float));
      }
    }
    return true;
  }
};

// A PanelSource the worker must never call (Files mode reads panels itself).
struct NeverSource : mmm::PanelSource {
  bool fill_band(uint32_t, uint64_t, uint64_t, float*) override {
    CHECK(false);  // Files mode must not issue BandRequests.
    return false;
  }
};

// Buffers streamed output bands into one planar (c*H + y)*W + x mosaic.
struct BufCollector : mmm::OutputCollector {
  uint64_t W = 0, H = 0, C = 0;
  std::vector<float> data;
  void begin(uint64_t w_, uint64_t h_, uint64_t c_) override {
    W = w_;
    H = h_;
    C = c_;
    data.assign(w_ * h_ * c_, 0.f);
  }
  void band(uint64_t y0, uint64_t rows, const float* p, uint64_t width, uint64_t c) override {
    for (uint64_t ch = 0; ch < c; ch++) {
      for (uint64_t r = 0; r < rows; r++) {
        std::memcpy(&data[(ch * H + y0 + r) * width], p + (ch * rows + r) * width,
                    width * sizeof(float));
      }
    }
  }
};

// Builds the `params` object mirroring end_to_end.rs::blend_params.
json make_params(uint32_t band_rows, double feather_px) {
  json p;
  p["feather_px"] = feather_px;
  p["downsample"] = 1;
  p["band_rows"] = band_rows;
  p["mode"] = "pyramid";
  p["roi"] = nullptr;
  p["defect_veto"] = true;
  p["flatten"] = nullptr;
  p["surface_order"] = 2;
  return p;
}

json make_panels(const json& meta) {
  json panels = json::array();
  for (const auto& p : meta.at("panels")) {
    json pd;
    pd["panel_id"] = p.at("id").get<uint32_t>();
    pd["width"] = p.at("w").get<uint64_t>();
    pd["height"] = p.at("h").get<uint64_t>();
    pd["channels"] = p.at("ch").get<uint64_t>();
    pd["properties"] = json::array();
    panels.push_back(pd);
  }
  return panels;
}

// One run's result: the collected mosaic plus the geometry the worker
// announced via Begin (the output canvas is the data bounding box, which the
// worker computes -- not necessarily the full Init canvas).
struct RunResult {
  std::vector<float> data;
  uint64_t w = 0, h = 0, ch = 0;
};

// Runs one job to completion and returns the collected mosaic + geometry.
RunResult run_job(const std::string& worker_path, const json& init_body,
                  const mmm::SlotLayout& layout, const std::string& shm_name,
                  mmm::PanelSource& src) {
  mmm::HostConfig cfg;
  cfg.worker_path = worker_path;
  cfg.layout = layout;
  cfg.shm_name = shm_name;
  cfg.init = json{{"Init", init_body}};

  BufCollector out;
  mmm::Host host(std::move(cfg), src, out);
  host.run();
  CHECK(!host.cancelled());
  return RunResult{std::move(out.data), out.W, out.H, out.C};
}

}  // namespace

int main(int argc, char** argv) {
  CHECK(argc >= 3);
  const std::string fixtures = argv[1];
  const std::string worker_path = argv[2];

  json meta = json::parse(read_bytes(fixtures + "/meta.json"));
  const uint64_t w = meta.at("canvas")[0];
  const uint64_t h = meta.at("canvas")[1];
  const uint64_t ch = meta.at("canvas")[2];
  const uint32_t band_rows = meta.at("band_rows").get<uint32_t>();
  const double feather_px = meta.at("feather_px").get<double>();
  const size_t n_panels = meta.at("panels").size();

  // shm slots sized to one band's worth of canvas pixels.
  const uint64_t slot_bytes = w * ch * band_rows * 4;
  mmm::SlotLayout layout{slot_bytes, 8, 2};

  const int pid = static_cast<int>(::getpid());
  json panels = make_panels(meta);
  json params = make_params(band_rows, feather_px);

  // ---- Run 1: Files-mode golden. Worker reads the .xisf files itself. ----
  json files_paths = json::array();
  for (size_t i = 0; i < n_panels; i++) {
    files_paths.push_back(fixtures + "/panel" + std::to_string(i) + ".xisf");
  }
  json files_init;
  files_init["protocol_version"] = 2;
  files_init["shm_name"] = "";  // filled below
  files_init["slot_bytes"] = slot_bytes;
  files_init["input_slots"] = layout.input_slots;
  files_init["output_slots"] = layout.output_slots;
  files_init["canvas"] = {w, h, ch};
  files_init["panels"] = panels;
  files_init["mode"] = json{{"Files", {{"paths", files_paths}, {"input_select", "Auto"}}}};
  files_init["session_dir"] = fixtures + "/files_" + std::to_string(pid) + ".mmm-session";
  files_init["params"] = params;

  const std::string files_shm = "/mmm-golden-files-" + std::to_string(pid);
  files_init["shm_name"] = files_shm;
  NeverSource never;
  RunResult golden_run = run_job(worker_path, files_init, layout, files_shm, never);
  const std::vector<float>& golden = golden_run.data;

  // ---- Run 2: Aligned-mode. Our MemSource serves panelN.bin over shm. ----
  MemSource mem;
  for (size_t i = 0; i < n_panels; i++) {
    const auto& pj = meta.at("panels")[i];
    uint64_t pw = pj.at("w"), ph = pj.at("h"), pc = pj.at("ch");
    mem.w.push_back(pw);
    mem.h.push_back(ph);
    mem.ch.push_back(pc);
    mem.panels.push_back(
        read_floats(fixtures + "/panel" + std::to_string(i) + ".bin", pw * ph * pc));
  }

  json aligned_init = files_init;
  aligned_init["mode"] = "Aligned";
  aligned_init["session_dir"] = fixtures + "/aligned_" + std::to_string(pid) + ".mmm-session";
  const std::string aligned_shm = "/mmm-golden-aligned-" + std::to_string(pid);
  aligned_init["shm_name"] = aligned_shm;
  RunResult got_run = run_job(worker_path, aligned_init, layout, aligned_shm, mem);
  const std::vector<float>& got = got_run.data;

  // ---- Assertions. ----
  // The output canvas is the blend's data bounding box (worker-computed), not
  // the full Init canvas; both runs must agree on it.
  CHECK(golden_run.w == got_run.w);
  CHECK(golden_run.h == got_run.h);
  CHECK(golden_run.ch == got_run.ch);
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

  // Byte-identity: aligned shm output == files-mode golden, element for element.
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

  std::printf("test_golden_aligned OK: %zu elements byte-identical (range [%.6g, %.6g])\n",
              got.size(), static_cast<double>(lo), static_cast<double>(hi));
  return 0;
}
