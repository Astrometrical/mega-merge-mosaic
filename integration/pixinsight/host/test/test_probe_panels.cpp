// Host::probe_panels test -- the Files-mode metadata probe (PROTOCOL.md §11).
//
// Drives the real `mmm-ipc-worker --probe-panels` through the C++ host and
// checks, against the generated fixtures:
//   1. solved raw panels (solved0/1.xisf, Auto): per-panel geometry matches
//      solved_meta.json and the reported frame is byte-for-byte the same
//      `choose_frame` result `Host::probe_frame` returns for the same panels
//      -- the slot-sizing parity a real PixInsight host depends on;
//   2. input_select "Aligned" suppresses the frame;
//   3. registered aligned panels (panel0/1.xisf, no solutions, Auto): correct
//      geometry, no frame;
//   4. a nonexistent path fails with a HostError that names the file --
//      proving the probe helper captures the worker's stderr.

#include <cstdint>
#include <fstream>
#include <iterator>
#include <string>
#include <vector>

#include "mmm_host.h"
#include "test/test_util.h"
#include "third_party/json.hpp"

namespace {

using nlohmann::json;

json parse_file(const std::string& path) {
  std::ifstream f(path, std::ios::binary);
  CHECK(f.good());
  std::string text((std::istreambuf_iterator<char>(f)), std::istreambuf_iterator<char>());
  return json::parse(text);
}

// Counts on_idle ticks; never cancels. Exercises the prog!=nullptr path of
// the probe drain loop (tiny fixtures may finish without a single tick, so
// the count itself is not asserted).
struct IdleCounter : mmm::ProgressCallback {
  int idles = 0;
  void on_idle() override { idles++; }
};

}  // namespace

int main(int argc, char** argv) {
  CHECK(argc >= 3);
  const std::string fixtures = argv[1];
  const std::string worker_path = argv[2];

  json meta = parse_file(fixtures + "/solved_meta.json");
  json props = parse_file(fixtures + "/solved_props.json");
  const uint64_t ch = meta.at("ch").get<uint64_t>();
  const size_t n_panels = meta.at("panels").size();
  CHECK(props.is_array());
  CHECK(props.size() == n_panels);

  // Expected frame via the existing probe_frame path (panels + spliced
  // properties, mode Solved) -- exactly as test_golden_solved sizes its run.
  json probe_init;
  // protocol_version + worker_version are stamped by Host (run/probe) itself.
  probe_init["shm_name"] = "";
  probe_init["slot_bytes"] = 0;
  probe_init["input_slots"] = 0;
  probe_init["output_slots"] = 0;
  probe_init["canvas"] = {0, 0, ch};
  {
    json panels = json::array();
    size_t i = 0;
    for (const auto& p : meta.at("panels")) {
      json pd;
      pd["panel_id"] = p.at("id").get<uint32_t>();
      pd["width"] = p.at("w").get<uint64_t>();
      pd["height"] = p.at("h").get<uint64_t>();
      pd["channels"] = p.at("ch").get<uint64_t>();
      pd["properties"] = props[i];
      panels.push_back(pd);
      i++;
    }
    probe_init["panels"] = panels;
  }
  probe_init["mode"] = "Solved";
  probe_init["session_dir"] = "";
  {
    json params;
    params["feather_px"] = meta.at("feather_px").get<double>();
    params["downsample"] = 1;
    params["band_rows"] = meta.at("band_rows").get<uint32_t>();
    params["mode"] = "pyramid";
    params["roi"] = nullptr;
    params["defect_veto"] = true;
    params["flatten"] = nullptr;
    params["surface_order"] = 2;
    probe_init["params"] = params;
  }
  uint64_t frame_w = 0, frame_h = 0, frame_ch = 0;
  mmm::Host::probe_frame(worker_path, probe_init, frame_w, frame_h, frame_ch);
  CHECK(frame_w > 0 && frame_h > 0 && frame_ch == ch);

  // ---- 1. Solved raw panels, Auto: geometry + the same frame. ----
  std::vector<std::string> solved_paths;
  for (size_t i = 0; i < n_panels; i++) {
    solved_paths.push_back(fixtures + "/solved" + std::to_string(i) + ".xisf");
  }
  IdleCounter idle;
  mmm::PanelProbeResult res = mmm::Host::probe_panels(worker_path, solved_paths, "Auto", &idle);
  CHECK(res.panels.size() == n_panels);
  {
    size_t i = 0;
    for (const auto& p : meta.at("panels")) {
      CHECK(res.panels[i].width == p.at("w").get<uint64_t>());
      CHECK(res.panels[i].height == p.at("h").get<uint64_t>());
      CHECK(res.panels[i].channels == p.at("ch").get<uint64_t>());
      i++;
    }
  }
  CHECK(res.has_frame);
  CHECK(res.frame_w == frame_w);
  CHECK(res.frame_h == frame_h);
  CHECK(res.frame_ch == frame_ch);

  // ---- 2. input_select Aligned suppresses the frame. ----
  mmm::PanelProbeResult aligned_sel =
      mmm::Host::probe_panels(worker_path, solved_paths, "Aligned", nullptr);
  CHECK(!aligned_sel.has_frame);
  CHECK(aligned_sel.panels.size() == n_panels);

  // ---- 3. Registered aligned panels (no solutions), Auto: no frame. ----
  json ameta = parse_file(fixtures + "/meta.json");
  std::vector<std::string> aligned_paths;
  for (size_t i = 0; i < ameta.at("panels").size(); i++) {
    aligned_paths.push_back(fixtures + "/panel" + std::to_string(i) + ".xisf");
  }
  mmm::PanelProbeResult ares =
      mmm::Host::probe_panels(worker_path, aligned_paths, "Auto", nullptr);
  CHECK(!ares.has_frame);
  CHECK(ares.panels.size() == ameta.at("panels").size());
  {
    size_t i = 0;
    for (const auto& p : ameta.at("panels")) {
      CHECK(ares.panels[i].width == p.at("w").get<uint64_t>());
      CHECK(ares.panels[i].height == p.at("h").get<uint64_t>());
      CHECK(ares.panels[i].channels == p.at("ch").get<uint64_t>());
      i++;
    }
  }

  // ---- 4. A missing file fails with its name in the message (stderr
  // capture) rather than a bare "exited abnormally". ----
  std::vector<std::string> bad_paths = solved_paths;
  bad_paths.push_back(fixtures + "/nope.xisf");
  bool threw = false;
  try {
    (void)mmm::Host::probe_panels(worker_path, bad_paths, "Auto", nullptr);
  } catch (const mmm::HostError& e) {
    threw = true;
    CHECK(std::string(e.what()).find("nope.xisf") != std::string::npos);
  }
  CHECK(threw);

  std::fprintf(stderr, "test_probe_panels: OK (%d idle ticks on run 1)\n", idle.idles);
  return 0;
}
