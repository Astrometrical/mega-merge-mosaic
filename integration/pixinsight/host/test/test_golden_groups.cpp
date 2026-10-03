// Two-group run through the host: the reference derived over every group's
// panels is imposed on each group's Solved job, so both outputs land on the
// same grid even though group B is a single panel whose own frame would be
// much smaller. Mirrors what the PixInsight module does per group.

#include <algorithm>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <fstream>
#include <iterator>
#include <string>
#include <vector>

#include "mmm_host.h"
#include "test/golden_harness.h"
#include "test/test_util.h"
#include "third_party/json.hpp"

namespace {
using nlohmann::json;
using mmm_test::make_params;
using mmm_test::MemSource;
using mmm_test::run_job;
using mmm_test::RunResult;

std::string read_text(const std::string& path) {
  std::ifstream f(path, std::ios::binary);
  CHECK(f.good());
  return std::string((std::istreambuf_iterator<char>(f)), std::istreambuf_iterator<char>());
}

std::vector<float> read_floats(const std::string& path, uint64_t count) {
  std::ifstream f(path, std::ios::binary);
  CHECK(f.good());
  std::vector<float> out(count);
  f.read(reinterpret_cast<char*>(out.data()), count * sizeof(float));
  CHECK(f.gcount() == static_cast<std::streamsize>(count * sizeof(float)));
  return out;
}

json desc(const json& meta_panel, const json& props, uint32_t id) {
  json pd;
  pd["panel_id"] = id;
  pd["width"] = meta_panel.at("w").get<uint64_t>();
  pd["height"] = meta_panel.at("h").get<uint64_t>();
  pd["channels"] = meta_panel.at("ch").get<uint64_t>();
  pd["properties"] = props;
  return pd;
}
}  // namespace

int main(int argc, char** argv) {
  CHECK(argc >= 3);
  const std::string fixtures = argv[1];
  const std::string worker_path = argv[2];
  json meta = json::parse(read_text(fixtures + "/solved_meta.json"));
  json props = json::parse(read_text(fixtures + "/solved_props.json"));
  const uint64_t ch = meta.at("ch").get<uint64_t>();
  const uint32_t band_rows = meta.at("band_rows").get<uint32_t>();
  json params = make_params(band_rows, meta.at("feather_px").get<double>());
  const int pid = mmm_test_getpid();

  // Groups: A = {solved0, solved1}, B = {solved0}.
  std::vector<std::vector<size_t>> groups = {{0, 1}, {0}};

  // Reference over every panel of every group (ids renumbered 0..n).
  json all = json::array();
  uint32_t id = 0;
  for (const auto& g : groups)
    for (size_t k : g) all.push_back(desc(meta.at("panels")[k], props[k], id++));
  json probe_init;
  probe_init["shm_name"] = "";
  probe_init["slot_bytes"] = 0;
  probe_init["input_slots"] = 0;
  probe_init["output_slots"] = 0;
  probe_init["canvas"] = {0, 0, ch};
  probe_init["panels"] = all;
  probe_init["mode"] = "Solved";
  probe_init["session_dir"] = "";
  probe_init["params"] = params;
  json reference = mmm::Host::probe_reference(worker_path, probe_init);
  uint64_t rw = 0, rh = 0;
  CHECK(mmm::reference_canvas(reference, rw, rh));

  uint64_t max_w = 0;
  for (const auto& p : meta.at("panels")) max_w = std::max<uint64_t>(max_w, p.at("w").get<uint64_t>());
  const uint64_t slot_bytes = std::max<uint64_t>(max_w, rw) * ch * band_rows * 4;
  mmm::SlotLayout layout{slot_bytes, 8, 2};

  std::vector<RunResult> outputs;
  for (size_t gi = 0; gi < groups.size(); gi++) {
    MemSource mem;
    json panels = json::array();
    uint32_t pid_in_group = 0;
    for (size_t k : groups[gi]) {
      const auto& pj = meta.at("panels")[k];
      uint64_t pw = pj.at("w"), ph = pj.at("h"), pc = pj.at("ch");
      mem.w.push_back(pw);
      mem.h.push_back(ph);
      mem.ch.push_back(pc);
      mem.panels.push_back(read_floats(fixtures + "/solved" + std::to_string(k) + ".bin", pw * ph * pc));
      panels.push_back(desc(pj, props[k], pid_in_group++));
    }
    json init;
    init["slot_bytes"] = slot_bytes;
    init["input_slots"] = layout.input_slots;
    init["output_slots"] = layout.output_slots;
    init["canvas"] = {0, 0, ch};
    init["panels"] = panels;
    init["mode"] = "Solved";
    init["session_dir"] = fixtures + "/groups_" + std::to_string(gi) + "_" + std::to_string(pid) + ".mmm-session";
    init["params"] = params;
    init["reference"] = reference;
    const std::string shm = "/mmm-golden-groups-" + std::to_string(gi) + "-" + std::to_string(pid);
    init["shm_name"] = shm;
    outputs.push_back(run_job(worker_path, init, layout, shm, mem));
  }

  // Both outputs are the whole reference frame (canvas extent), hence equal.
  for (const auto& o : outputs) {
    CHECK(o.w == rw);
    CHECK(o.h == rh);
    CHECK(o.ch == ch);
  }
  // Group B (one panel) is non-empty and differs from group A.
  bool any_nonzero = false;
  for (float v : outputs[1].data) any_nonzero |= (v != 0.0f);
  CHECK(any_nonzero);
  CHECK(outputs[0].data != outputs[1].data);

  std::printf("test_golden_groups OK: 2 groups on a %llu x %llu reference grid\n",
              static_cast<unsigned long long>(rw), static_cast<unsigned long long>(rh));
  return 0;
}
