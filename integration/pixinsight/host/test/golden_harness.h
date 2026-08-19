#pragma once
// Shared test harness for the golden byte-identity tests (test_golden_aligned,
// test_golden_solved) and the fault-isolation test (test_isolation): an
// in-memory PanelSource/OutputCollector pair plus the run_job/make_params
// helpers used to drive `mmm::Host` end to end over the real worker binary.
//
// Extracted (Task 9) out of verbatim copies in test_golden_aligned.cpp and
// test_golden_solved.cpp before adding a third consumer that would otherwise
// triplicate them. Free functions are `inline` (not `static`/anonymous-
// namespace) so a consumer that only uses some of these entities does not
// trigger -Wunused-function in its own translation unit.

#include <cstdint>
#include <cstring>
#include <set>
#include <string>
#include <vector>

#include "mmm_host.h"
#include "test/test_util.h"
#include "third_party/json.hpp"

namespace mmm_test {

using nlohmann::json;

// Serves band requests from in-memory planar panels. Planar layout channels x
// height x width; fill_band copies rows [y0,y1) into the slot in the band's
// planar layout: index(c,r,x) = c*rows*w + r*w + x.
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
inline json make_params(uint32_t band_rows, double feather_px) {
  json p;
  p["feather_px"] = feather_px;
  p["downsample"] = 1;
  p["band_rows"] = band_rows;
  p["mode"] = "pyramid";
  p["roi"] = nullptr;
  p["defect_veto"] = true;
  p["flatten"] = nullptr;
  p["surface_order"] = 2;
  p["gain"] = "fit";
  return p;
}

// One run's result: the collected mosaic plus the geometry the worker
// announced via Begin (the output canvas is the data bounding box, which the
// worker computes -- not necessarily the full Init canvas).
struct RunResult {
  std::vector<float> data;
  uint64_t w = 0, h = 0, ch = 0;
};

// Runs one job to completion and returns the collected mosaic + geometry.
// `prog` optionally observes Progress frames (e.g. StageRecorder below).
inline RunResult run_job(const std::string& worker_path, const json& init_body,
                        const mmm::SlotLayout& layout, const std::string& shm_name,
                        mmm::PanelSource& src, mmm::ProgressCallback* prog = nullptr) {
  mmm::HostConfig cfg;
  cfg.worker_path = worker_path;
  cfg.layout = layout;
  cfg.shm_name = shm_name;
  cfg.init = json{{"Init", init_body}};

  BufCollector out;
  mmm::Host host(std::move(cfg), src, out, prog);
  host.run();
  CHECK(!host.cancelled());
  return RunResult{std::move(out.data), out.W, out.H, out.C};
}

// Records which Progress stages the worker announced.
struct StageRecorder : mmm::ProgressCallback {
  std::set<std::string> stages;
  void on_progress(const std::string& stage, uint64_t, uint64_t) override {
    stages.insert(stage);
  }
};

}  // namespace mmm_test
