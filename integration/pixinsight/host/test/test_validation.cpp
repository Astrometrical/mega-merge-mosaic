// Host-side wire-value validation tests: `mmm::Host::run()` must refuse a
// worker whose Begin/OutputBand/BandRequest values are out of range with a
// clean HostError -- BEFORE any memcpy against the collector or a shm slot
// runs. Without these checks a misbehaving worker (version skew, memory
// corruption, a plain bug) makes the host write past the output image's
// channel planes or past the shm mapping: silent heap corruption inside the
// embedding application (PixInsight) that detonates long after the run.
//
// The misbehaving worker is `rogue_worker` (rogue_worker.cpp, argv[1] =
// scenario), a scripted binary that emits exactly the frames each scenario
// names. Its "valid" scenario completes a run normally, proving the rogue's
// framing is sound -- so an attack scenario that fails indicts the host's
// validation, not the encoding.

#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <string>
#include <thread>
#include <vector>

#include "mmm_host.h"
#include "test/golden_harness.h"
#include "test/test_util.h"
#include "third_party/json.hpp"

namespace {

using nlohmann::json;

void start_watchdog(std::chrono::seconds timeout) {
  std::thread([timeout] {
    std::this_thread::sleep_for(timeout);
    std::fprintf(stderr, "WATCHDOG: test_validation did not finish within %lds -- aborting.\n",
                 static_cast<long>(timeout.count()));
    std::fflush(stderr);
    std::_Exit(1);
  }).detach();
}

// Counts fill_band calls; never touches the slot. The dangerous request
// scenarios assert the host refused BEFORE consulting the source at all.
struct CountingSource : mmm::PanelSource {
  int calls = 0;
  bool fill_band(uint32_t, uint64_t, uint64_t, float*) override {
    calls++;
    return true;
  }
};

// Counts bands without buffering. Attack scenarios assert no band ever
// reached the collector.
struct CountingCollector : mmm::OutputCollector {
  bool began = false;
  uint64_t w = 0, h = 0, c = 0, bands = 0;
  void begin(uint64_t w_, uint64_t h_, uint64_t c_) override {
    began = true;
    w = w_;
    h = h_;
    c = c_;
  }
  void band(uint64_t, uint64_t, const float*, uint64_t, uint64_t) override { bands++; }
};

struct Outcome {
  bool threw = false;
  std::string message;
  bool began = false;
  uint64_t w = 0, h = 0, c = 0;
  uint64_t bands = 0;
  int fills = 0;
};

// Drives one rogue scenario through the real Host::run(). The slot layout is
// fixed: slot_bytes = 32 px * 1 ch * 8 rows * 4 B = 1024, 4 input slots, 2
// output slots. `canvas` is the Init canvas; panels always declare 32x32x1
// (what the request scenarios' bounds are checked against).
Outcome run_scenario(const std::string& rogue_path, const std::string& scenario,
                     std::vector<uint64_t> canvas) {
  const uint64_t slot_bytes = 32 * 1 * 8 * 4;
  mmm::SlotLayout layout{slot_bytes, 4, 2};
  const std::string shm_name =
      "/mmm-validation-" + scenario + "-" + std::to_string(mmm_test_getpid());

  json panels = json::array();
  for (uint32_t id = 0; id < 2; id++) {
    json pd;
    pd["panel_id"] = id;
    pd["width"] = 32;
    pd["height"] = 32;
    pd["channels"] = 1;
    pd["properties"] = json::array();
    panels.push_back(pd);
  }
  json init;
  // protocol_version + worker_version are stamped by Host itself.
  init["shm_name"] = shm_name;
  init["slot_bytes"] = slot_bytes;
  init["input_slots"] = layout.input_slots;
  init["output_slots"] = layout.output_slots;
  init["canvas"] = canvas;
  init["panels"] = panels;
  init["mode"] = "Aligned";
  init["session_dir"] = "";  // the rogue never reads Init, let alone a session
  init["params"] = mmm_test::make_params(8, 8.0);

  mmm::HostConfig cfg;
  cfg.worker_path = rogue_path;
  cfg.worker_args = {scenario};
  cfg.layout = layout;
  cfg.shm_name = shm_name;
  cfg.init = json{{"Init", init}};

  CountingSource src;
  CountingCollector col;
  mmm::Host host(std::move(cfg), src, col);

  Outcome out;
  try {
    host.run();
  } catch (const mmm::HostError& e) {
    out.threw = true;
    out.message = e.what();
  }
  out.began = col.began;
  out.w = col.w;
  out.h = col.h;
  out.c = col.c;
  out.bands = col.bands;
  out.fills = src.calls;
  std::fprintf(stderr, "  %-22s -> %s%s\n", scenario.c_str(),
               out.threw ? "HostError: " : "completed",
               out.threw ? out.message.c_str() : "");
  return out;
}

void expect_refused(const Outcome& out, const char* needle) {
  CHECK(out.threw);
  CHECK(out.message.find(needle) != std::string::npos);
}

}  // namespace

int main(int argc, char** argv) {
  CHECK(argc >= 2);
  const std::string rogue = argv[1];
  start_watchdog(std::chrono::seconds(60));

  // Control: the rogue's framing drives a normal run to completion.
  {
    Outcome out = run_scenario(rogue, "valid", {32, 32, 1});
    CHECK(!out.threw);
    CHECK(out.began);
    CHECK(out.w == 32 && out.h == 32 && out.c == 1);
    CHECK(out.bands == 1);
  }

  // --- OutputBand attacks ---------------------------------------------------
  {
    Outcome out = run_scenario(rogue, "band_before_begin", {32, 32, 1});
    expect_refused(out, "before Begin");
    CHECK(out.bands == 0);
  }
  {
    // y0 + rows > Begin h: the memcpy would run past the output image's
    // channel plane -- the exact heap-corruption shape this suite pins down.
    Outcome out = run_scenario(rogue, "band_past_end", {32, 32, 1});
    expect_refused(out, "past the canvas");
    CHECK(out.bands == 0);
  }
  {
    Outcome out = run_scenario(rogue, "band_slot_oob", {32, 32, 1});
    expect_refused(out, "output slot");
    CHECK(out.bands == 0);
  }
  {
    // In canvas range but bigger than one slot: reading it would overrun the
    // shm mapping.
    Outcome out = run_scenario(rogue, "band_overflows_slot", {32, 1024, 1});
    expect_refused(out, "exceeds the slot");
    CHECK(out.bands == 0);
  }

  // --- Begin attacks --------------------------------------------------------
  {
    Outcome out = run_scenario(rogue, "begin_twice", {32, 32, 1});
    expect_refused(out, "duplicate Begin");
  }
  {
    Outcome out = run_scenario(rogue, "begin_mismatch", {32, 32, 1});
    expect_refused(out, "canvas");
    CHECK(!out.began);  // refused before reaching the collector
  }
  {
    // Solved-style Init (canvas w/h unknown): zero dimensions still refused.
    Outcome out = run_scenario(rogue, "begin_zero", {0, 0, 1});
    expect_refused(out, "empty");
    CHECK(!out.began);
  }
  {
    // Beyond int32: would truncate in the collector's ImageWindow(int, ...).
    Outcome out = run_scenario(rogue, "begin_huge", {0, 0, 1});
    expect_refused(out, "too large");
    CHECK(!out.began);
  }

  // --- BandRequest attacks --------------------------------------------------
  {
    Outcome out = run_scenario(rogue, "request_slot_oob", {32, 32, 1});
    expect_refused(out, "input slot");
    CHECK(out.fills == 0);  // refused before the PanelSource ran
  }
  {
    // In panel range but bigger than one slot: the fill would overrun the
    // shm mapping. Must be refused BEFORE the PanelSource writes anything.
    Outcome out = run_scenario(rogue, "request_overflows_slot", {32, 32, 1});
    expect_refused(out, "exceeds the slot");
    CHECK(out.fills == 0);
  }

  std::printf("test_validation OK: rogue Begin/OutputBand/BandRequest values "
              "are refused with clean HostErrors before any memcpy\n");
  return 0;
}
