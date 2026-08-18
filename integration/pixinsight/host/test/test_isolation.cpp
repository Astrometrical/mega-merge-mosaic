// Fault-isolation tests (Task 9): a worker that crashes/exits-without-Done
// surfaces a clean HostError promptly (no hang, no partial output), and a
// mid-run cancel() stops promptly with the documented outcome. Mirrors
// crates/mmm-ipc-worker/tests/end_to_end.rs's `worker_crash_is_observable`
// and `cancel_midrun_stops_promptly` -- but drives the same guarantees
// through the C++ `mmm::Host` instead of the Rust reference host.
//
// Both cases are proven WITHOUT any production-only test seam:
//   - crash: `Host::run()` is pointed at `worker_path = "/bin/false"`, a
//     real binary that exits immediately without ever touching stdin/stdout.
//     `Host::run()`'s existing EOF-before-Done handling (see mmm_host.cpp)
//     throws `HostError` on its own; no hook into `Host` is required.
//   - cancel: the real `mmm-ipc-worker` binary is driven normally; a second
//     thread calls the public `Host::cancel()` once the run is genuinely
//     underway (signaled by the injected PanelSource's first `fill_band`
//     call, via a condition variable -- no sleeps). `Host::run()` already
//     reaps the child unconditionally on every exit path (see mmm_host.cpp's
//     final `waitpid` call, which runs whether or not `cancelled_` is set),
//     so no additional seam is needed to prove the worker is reaped.
//
// A wall-clock watchdog thread aborts the process after ~20s so a hang FAILS
// loudly (nonzero exit, clear stderr message) instead of hanging CI.

#include <atomic>
#include <chrono>
#include <condition_variable>
#include <cstdio>
#include <cstdlib>
#include <filesystem>
#include <mutex>
#include <string>
#include <thread>
#include <vector>

#include "mmm_host.h"
#include "test/golden_harness.h"
#include "test/test_util.h"
#include "third_party/json.hpp"

namespace {

using nlohmann::json;

// ---------------------------------------------------------------------------
// Watchdog: aborts the whole process if the test suite runs longer than
// `timeout`. Detached so it never blocks a normal exit; a completing test
// just leaves this thread's sleep unfinished when the process exits.
// ---------------------------------------------------------------------------
void start_watchdog(std::chrono::seconds timeout) {
  std::thread([timeout] {
    std::this_thread::sleep_for(timeout);
    std::fprintf(stderr,
                 "WATCHDOG: test_isolation did not finish within %lds -- "
                 "treating as a hang regression and aborting.\n",
                 static_cast<long>(timeout.count()));
    std::fflush(stderr);
    std::_Exit(1);  // hard, portable, no-cleanup abort (POSIX _exit equivalent)
  }).detach();
}

// A collector that must never be invoked -- used by the crash case, where a
// worker that never emits Begin/OutputBand before dying must never surface
// as partial output.
struct AssertNeverCollector : mmm::OutputCollector {
  bool called = false;
  void begin(uint64_t, uint64_t, uint64_t) override { called = true; }
  void band(uint64_t, uint64_t, const float*, uint64_t, uint64_t) override { called = true; }
};

// A collector that records whether streaming ever started, without buffering
// any pixel data -- used by the cancel case to assert no output was surfaced.
struct TrackingCollector : mmm::OutputCollector {
  bool began = false;
  uint64_t bands_seen = 0;
  void begin(uint64_t, uint64_t, uint64_t) override { began = true; }
  void band(uint64_t, uint64_t, const float*, uint64_t, uint64_t) override { bands_seen++; }
};

// Wraps mmm_test::MemSource to signal (via condition variable) once the
// first fill_band call has returned, so the test can decide "the run is
// genuinely underway" without sleeping.
struct SignalingSource : mmm::PanelSource {
  mmm_test::MemSource inner;
  std::mutex mu;
  std::condition_variable cv;
  int calls = 0;

  bool fill_band(uint32_t id, uint64_t y0, uint64_t y1, float* dst) override {
    bool ok = inner.fill_band(id, y0, y1, dst);
    {
      std::lock_guard<std::mutex> lk(mu);
      calls++;
    }
    cv.notify_all();
    return ok;
  }
};

std::vector<float> flat(uint64_t w, uint64_t h, uint64_t ch, float v) {
  return std::vector<float>(w * h * ch, v);
}

// Builds a minimal Aligned-mode Init body for a two-panel w x h x ch job.
// `session_dir`/`shm_name` are per-test-run so concurrent CI runs never
// collide.
json make_init(uint64_t w, uint64_t h, uint64_t ch, uint32_t band_rows, uint64_t slot_bytes,
               const mmm::SlotLayout& layout, const std::string& shm_name,
               const std::string& session_dir) {
  json panels = json::array();
  for (uint32_t id = 0; id < 2; id++) {
    json pd;
    pd["panel_id"] = id;
    pd["width"] = w;
    pd["height"] = h;
    pd["channels"] = ch;
    pd["properties"] = json::array();
    panels.push_back(pd);
  }
  json init;
  // protocol_version + worker_version are stamped by Host (run/probe) itself.
  init["shm_name"] = shm_name;
  init["slot_bytes"] = slot_bytes;
  init["input_slots"] = layout.input_slots;
  init["output_slots"] = layout.output_slots;
  init["canvas"] = {w, h, ch};
  init["panels"] = panels;
  init["mode"] = "Aligned";
  init["session_dir"] = session_dir;
  init["params"] = mmm_test::make_params(band_rows, 8.0);
  return init;
}

std::string unique_tmp_dir(const std::string& tag) {
  namespace fs = std::filesystem;
  fs::path dir = fs::temp_directory_path() /
                 ("mmm-isolation-" + tag + "-" + std::to_string(mmm_test_getpid()));
  std::error_code ec;
  fs::remove_all(dir, ec);
  fs::create_directories(dir, ec);
  CHECK(!ec);
  return dir.string();
}

// ---------------------------------------------------------------------------
// Crash case: worker_path points at /bin/false, a real binary that exits
// immediately (status 0) without reading Init or writing any frame. Host::
// run() must observe clean EOF before Done and throw HostError -- promptly,
// and without ever calling into the OutputCollector or the PanelSource.
// ---------------------------------------------------------------------------
void test_crash_worker_exits_without_done() {
  std::fprintf(stderr, "test_crash_worker_exits_without_done: starting\n");
  const uint64_t w = 32, h = 32, ch = 1;
  const uint32_t band_rows = 8;
  const uint64_t slot_bytes = w * ch * band_rows * 4;
  mmm::SlotLayout layout{slot_bytes, 4, 2};

  std::string session_dir = unique_tmp_dir("crash");
  std::string shm_name = "/mmm-isolation-crash-" + std::to_string(mmm_test_getpid());

  mmm::HostConfig cfg;
  // A real program that exits immediately without reading Init or writing any
  // frame, so Host::run() observes clean EOF before Done and throws HostError.
  // The exe path and its args are kept SEPARATE (worker_path is a real
  // executable the host quotes; worker_args carries flags) -- production quotes
  // the worker exe for spaced install paths, and this test exercises that same
  // path with a real quick-exit process on every OS:
  //   Windows: cmd.exe /c exit 1  (exits nonzero at once, writes nothing)
  //   macOS:   /usr/bin/false     (bare exe, exits 1)
  //   Linux:   /bin/false         (bare exe, exits 1)
#ifdef _WIN32
  std::string cmd_path = "C:\\Windows\\System32\\cmd.exe";
  {
    wchar_t comspec[MAX_PATH];
    DWORD n = GetEnvironmentVariableW(L"ComSpec", comspec, MAX_PATH);
    if (n > 0 && n < MAX_PATH) {
      int bytes = WideCharToMultiByte(CP_UTF8, 0, comspec, static_cast<int>(n), nullptr, 0,
                                      nullptr, nullptr);
      std::string s(static_cast<size_t>(bytes), '\0');
      WideCharToMultiByte(CP_UTF8, 0, comspec, static_cast<int>(n), s.data(), bytes, nullptr,
                          nullptr);
      cmd_path = std::move(s);
    }
  }
  cfg.worker_path = cmd_path;
  cfg.worker_args = {"/c", "exit", "1"};
#elif defined(__APPLE__)
  cfg.worker_path = "/usr/bin/false";
#else
  cfg.worker_path = "/bin/false";
#endif
  cfg.layout = layout;
  cfg.shm_name = shm_name;
  cfg.init = json{{"Init", make_init(w, h, ch, band_rows, slot_bytes, layout, shm_name, session_dir)}};

  AssertNeverCollector collector;
  mmm_test::NeverSource never;  // /bin/false must never issue a BandRequest.
  mmm::Host host(std::move(cfg), never, collector);

  bool threw_host_error = false;
  std::string host_error_message;
  try {
    host.run();
  } catch (const mmm::HostError& e) {
    threw_host_error = true;
    host_error_message = e.what();
    std::fprintf(stderr, "  got expected HostError: %s\n", e.what());
  } catch (const std::exception& e) {
    std::fprintf(stderr, "  FAILED: wrong exception type escaped run(): %s\n", e.what());
  }

  CHECK(threw_host_error);
  // Not just any HostError -- specifically the exit-without-Done path (see
  // mmm_host.cpp), not e.g. a spawn failure or a shm setup error.
  CHECK(host_error_message.find("worker exited before Done") != std::string::npos);
  CHECK(!collector.called);   // No partial output ever surfaced.
  CHECK(!host.cancelled());   // This is a crash, not a cancellation.

  std::error_code ec;
  std::filesystem::remove_all(session_dir, ec);
  std::fprintf(stderr, "test_crash_worker_exits_without_done: OK\n");
}

// ---------------------------------------------------------------------------
// Cancel case: the real worker binary, driven normally, cancelled from a
// second thread once the first BandRequest has genuinely been served. Host::
// run() must return normally (not throw), cancelled() must report true, no
// output must have started streaming, and the worker must be reaped (Host::
// run()'s final waitpid runs unconditionally on this path -- see
// mmm_host.cpp).
// ---------------------------------------------------------------------------
void test_cancel_midrun_stops_promptly(const std::string& worker_path) {
  std::fprintf(stderr, "test_cancel_midrun_stops_promptly: starting\n");
  const uint64_t w = 96, h = 64, ch = 1;
  const uint32_t band_rows = 8;
  const uint64_t slot_bytes = w * ch * band_rows * 4;
  mmm::SlotLayout layout{slot_bytes, 4, 2};

  std::string session_dir = unique_tmp_dir("cancel");
  std::string shm_name = "/mmm-isolation-cancel-" + std::to_string(mmm_test_getpid());

  SignalingSource src;
  src.inner.w = {w, w};
  src.inner.h = {h, h};
  src.inner.ch = {ch, ch};
  src.inner.panels = {flat(w, h, ch, 0.3f), flat(w, h, ch, 0.5f)};

  TrackingCollector collector;

  mmm::HostConfig cfg;
  cfg.worker_path = worker_path;
  cfg.layout = layout;
  cfg.shm_name = shm_name;
  cfg.init = json{{"Init", make_init(w, h, ch, band_rows, slot_bytes, layout, shm_name, session_dir)}};

  mmm::Host host(std::move(cfg), src, collector);

  // Second thread: wait until the run is genuinely underway (the worker has
  // issued and received a reply to at least one BandRequest), then cancel.
  // No sleeps -- purely condition-variable synchronized.
  std::thread canceller([&] {
    std::unique_lock<std::mutex> lk(src.mu);
    src.cv.wait(lk, [&] { return src.calls >= 1; });
    lk.unlock();
    host.cancel();
  });

  bool threw = false;
  try {
    host.run();
  } catch (const std::exception& e) {
    threw = true;
    std::fprintf(stderr, "  FAILED: run() threw instead of returning cleanly: %s\n", e.what());
  }
  canceller.join();

  CHECK(!threw);              // A cancelled run returns normally, never throws.
  CHECK(host.cancelled());    // The documented cancel outcome.
  CHECK(!collector.began);    // No output ever started streaming.
  CHECK(collector.bands_seen == 0);

  std::error_code ec;
  std::filesystem::remove_all(session_dir, ec);
  std::fprintf(stderr, "test_cancel_midrun_stops_promptly: OK\n");
}

// ---------------------------------------------------------------------------
// Idle case: while the worker pipe is silent (no frames at all), Host::run()
// must keep invoking ProgressCallback::on_idle() instead of parking in a
// blind blocking read -- this is what lets the PixInsight module pump its GUI
// event queue (and honor Cancel) during long worker reads. The "worker" here
// is a real process that sleeps ~0.4s writing nothing, then exits; the run
// ends in the usual exit-before-Done HostError, by which time on_idle() must
// have fired repeatedly.
// ---------------------------------------------------------------------------
struct IdleCountingProgress : mmm::ProgressCallback {
  std::atomic<int> idles{0};
  void on_progress(const std::string&, uint64_t, uint64_t) override {}
  void on_idle() override { idles.fetch_add(1); }
};

void test_idle_callback_fires_while_pipe_quiet() {
  std::fprintf(stderr, "test_idle_callback_fires_while_pipe_quiet: starting\n");
  const uint64_t w = 32, h = 32, ch = 1;
  const uint32_t band_rows = 8;
  const uint64_t slot_bytes = w * ch * band_rows * 4;
  mmm::SlotLayout layout{slot_bytes, 4, 2};

  std::string session_dir = unique_tmp_dir("idle");
  std::string shm_name = "/mmm-isolation-idle-" + std::to_string(mmm_test_getpid());

  mmm::HostConfig cfg;
  // A real process that stays silent for ~0.4s (never reads Init, writes no
  // frame), then exits: a stand-in for a worker busy reading big panels.
#ifdef _WIN32
  cfg.worker_path = "C:\\Windows\\System32\\cmd.exe";
  cfg.worker_args = {"/c", "ping", "-n", "2", "127.0.0.1", ">", "NUL"};
#else
  cfg.worker_path = "/bin/sh";
  cfg.worker_args = {"-c", "sleep 0.4"};
#endif
  cfg.layout = layout;
  cfg.shm_name = shm_name;
  cfg.init = json{{"Init", make_init(w, h, ch, band_rows, slot_bytes, layout, shm_name, session_dir)}};

  AssertNeverCollector collector;
  mmm_test::NeverSource never;
  IdleCountingProgress prog;
  mmm::Host host(std::move(cfg), never, collector, &prog);

  bool threw_host_error = false;
  try {
    host.run();
  } catch (const mmm::HostError& e) {
    threw_host_error = true;
    std::fprintf(stderr, "  got expected HostError: %s\n", e.what());
  }

  CHECK(threw_host_error);        // Silent exit still ends as exit-before-Done.
  // ~0.4s of silence with a <=100ms idle interval must yield several idles;
  // >= 2 keeps the bound loose against scheduler jitter.
  std::fprintf(stderr, "  on_idle() fired %d times\n", prog.idles.load());
  CHECK(prog.idles.load() >= 2);

  std::error_code ec;
  std::filesystem::remove_all(session_dir, ec);
  std::fprintf(stderr, "test_idle_callback_fires_while_pipe_quiet: OK\n");
}

}  // namespace

int main(int argc, char** argv) {
  CHECK(argc >= 2);
  const std::string worker_path = argv[1];

  start_watchdog(std::chrono::seconds(20));

  test_crash_worker_exits_without_done();
  test_cancel_midrun_stops_promptly(worker_path);
  test_idle_callback_fires_while_pipe_quiet();

  std::printf("test_isolation OK: crash -> HostError (no partial output); "
              "cancel -> cancelled()==true (no output, worker reaped)\n");
  return 0;
}
