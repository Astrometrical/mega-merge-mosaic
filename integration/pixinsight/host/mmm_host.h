#pragma once
// The host-side worker driver: spawns `mmm-ipc-worker`, creates the POSIX
// shm segment, serves input bands on request, collects streamed output, and
// enforces fault isolation (a crashed/exited worker surfaces as a clean
// HostError, never a hang, never partial output). Mirrors the reference
// serve loop in crates/mmm-core/src/ipc/testhost.rs (`run_host`) and the
// handshake in crates/mmm-ipc-worker/tests/end_to_end.rs.
//
// NO PCL header here -- POSIX + stdlib + vendored nlohmann/json only -- so
// the golden test (test_golden_aligned) can drive the real worker without
// PixInsight installed.

#include <atomic>
#include <cstdint>
#include <mutex>
#include <stdexcept>
#include <string>
#include <vector>

#include "mmm_os.h"
#include "mmm_protocol.h"
#include "mmm_shm.h"
#include "third_party/json.hpp"

namespace mmm {

/// Where input pixels come from. The PCL module supplies rows from an
/// `ImageVariant`; tests supply rows from memory. Fill rows `[y0, y1)` of
/// every channel of panel `panel_id` into `dst` in planar native-endian
/// order: `index(c, r, x) = c*rows*w + r*w + x` (PROTOCOL.md Section 7, with
/// `rows = y1 - y0`). Return `false` to signal an out-of-range/failed fill
/// (the host then replies `status = 1` so the worker unwinds cleanly).
struct PanelSource {
  virtual ~PanelSource() = default;
  virtual bool fill_band(uint32_t panel_id, uint64_t y0, uint64_t y1, float* dst) = 0;
};

/// Receives output geometry then blended bands. The module writes bands into
/// an `ImageWindow`; tests buffer them. `planar` holds `ch*rows*width` f32
/// values in the same planar layout as an input band (`rows` = this band's
/// row count).
struct OutputCollector {
  virtual ~OutputCollector() = default;
  virtual void begin(uint64_t w, uint64_t h, uint64_t ch) = 0;
  virtual void band(uint64_t y0, uint64_t rows, const float* planar, uint64_t width,
                    uint64_t ch) = 0;
};

/// Optional progress observer, fed from the worker's `Progress` frames.
struct ProgressCallback {
  virtual ~ProgressCallback() = default;
  virtual void on_progress(const std::string& stage, uint64_t done, uint64_t total) {
    (void)stage;
    (void)done;
    (void)total;
  }
};

/// Everything needed to start one run. `init` is the full `{"Init":{...}}`
/// JSON object (per PROTOCOL.md Section 6), `layout` must agree with the
/// slot sizing inside `init`, and `shm_name` names the segment to create.
struct HostConfig {
  std::string worker_path;  // absolute path to mmm-ipc-worker (exact exe, no args)
  // Extra arguments passed to the worker after argv[0]. Production leaves this
  // empty (the worker takes no positional args in a normal run); tests use it
  // to spawn a real quick-exit process with its own flags. On Windows the exe
  // is quoted and each arg appended separately, so a spaced install path
  // (e.g. C:\Program Files\PixInsight\...) is handled correctly.
  std::vector<std::string> worker_args;
  nlohmann::json init;      // the full {"Init":{...}} object
  SlotLayout layout;        // must match init's shm sizing
  std::string shm_name;
};

/// Thrown by `Host::run`/`Host::probe_frame` on a worker crash, an
/// exit-before-`Done`, a nonzero exit, a protocol error, or a worker `Error`
/// frame that was not the result of a `cancel()`. A cancelled run does NOT
/// throw -- see `Host::run`.
struct HostError : std::runtime_error {
  using std::runtime_error::runtime_error;
};

/// Drives one blend job to completion over stdin/stdout + shm.
///
/// Fault isolation (spec Section 9 / PROTOCOL.md Section 10): a worker that
/// crashes, exits without `Done`, or exits nonzero surfaces as a `HostError`;
/// the shm segment is always unlinked (RAII via `ShmSegment`), and the
/// collector's partially-filled buffer is the caller's to discard on
/// exception (this class never presents partial output as success).
class Host {
 public:
  Host(HostConfig cfg, PanelSource& src, OutputCollector& out, ProgressCallback* prog = nullptr);

  /// Blocking: creates the shm, spawns the worker, sends `Init`, serves the
  /// request/output loop until `Done`, then reaps the child. Returns
  /// normally on `Done` OR on a clean cancellation (see `cancelled()`).
  /// Throws `HostError` on any fault.
  void run();

  /// Thread-safe: asks the worker to abort. Sends a `Cancel` frame (under the
  /// stdin mutex) and sets an atomic flag so the ensuing worker
  /// `Error{"cancelled"}` is treated as an intentional stop rather than a
  /// crash. Safe to call before `run` starts (the flag is honored once the
  /// worker exists) and from another thread while `run` is blocked.
  void cancel();

  /// True iff this run ended because of a `cancel()` (the worker acknowledged
  /// with `Error{"cancelled"}` after we requested cancellation). Meaningful
  /// after `run()` returns. Distinguishes a deliberate stop from a `Done`
  /// completion without conflating either with a crash (which throws).
  bool cancelled() const { return cancelled_.load(); }

  /// Solved-mode helper: spawn `worker_path --probe-frame`, write `init_obj`
  /// (an `InitJob`-shaped JSON object -- NOT wrapped in `{"Init":...}`) to
  /// its stdin, and parse the `"w h ch"` line it prints. Static, no shm.
  /// Throws `HostError` on a nonzero exit or unparseable output.
  static void probe_frame(const std::string& worker_path, const nlohmann::json& init_obj,
                          uint64_t& w, uint64_t& h, uint64_t& ch);

 private:
  // Writes a fully-framed byte buffer (tag+len+payload already prepended) to
  // the worker's stdin under `stdin_mutex_`.
  void write_framed_locked(const std::vector<uint8_t>& framed);

  HostConfig cfg_;
  PanelSource& src_;
  OutputCollector& out_;
  ProgressCallback* prog_;

  std::mutex stdin_mutex_;
  os_handle stdin_fd_ = os_invalid_handle;  // host -> worker; valid during run().
  std::atomic<bool> cancel_requested_{false};
  std::atomic<bool> cancelled_{false};

  uint64_t out_w_ = 0;
  uint64_t out_ch_ = 0;
};

}  // namespace mmm
