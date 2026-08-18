#include "../mmm_shm.h"
#include "test_util.h"
#include <cstring>
#include <stdexcept>
#include <string>

// A name ShmSegment::create must refuse on EVERY platform with a clear
// message: macOS caps POSIX shm names at 31 chars (PSHMNAMLEN) and fails
// longer ones with a bare EINVAL, so the portable guard is what keeps a
// Linux-developed test from only exploding on the macOS CI runner.
static void check_rejects(const std::string& name, const char* why) {
  bool threw = false;
  try {
    (void)mmm::ShmSegment::create(name, 4096);
  } catch (const std::runtime_error& e) {
    threw = true;
    std::string msg = e.what();
    CHECK(msg.find("shm name") != std::string::npos);
  }
  if (!threw) {
    std::fprintf(stderr, "create(%s) was not refused (%s)\n", name.c_str(), why);
    CHECK(false);
  }
}

int main() {
  using namespace mmm;
  SlotLayout L{ /*slot_bytes*/ 256, /*input*/ 4, /*output*/ 2 };
  CHECK(L.total_bytes() == 256ull * 6);
  CHECK(L.input_offset(0) == 0);
  CHECK(L.input_offset(3) == 256ull * 3);
  CHECK(L.output_offset(0) == 256ull * 4);
  CHECK(L.output_offset(1) == 256ull * 5);

  auto seg = ShmSegment::create("/mmm-cpp-test-shm", L.total_bytes());
  CHECK(seg.size() == L.total_bytes());
  // write floats into input slot 2, read them back
  float* p = seg.slot_floats(L.input_offset(2));
  for (int i = 0; i < 5; ++i) p[i] = float(i) + 0.5f;
  float* q = seg.slot_floats(L.input_offset(2));
  for (int i = 0; i < 5; ++i) CHECK(q[i] == float(i) + 0.5f);

  // Portable name validation (see check_rejects): over-31-chars (the macOS
  // PSHMNAMLEN limit, enforced everywhere so it cannot pass on Linux and
  // fail on macOS), no leading slash, embedded slash.
  check_rejects("/mmm-this-name-is-well-over-thirty-one-chars", "too long");
  check_rejects("mmm-no-leading-slash", "missing leading '/'");
  check_rejects("/mmm/embedded/slashes", "embedded slashes");

  std::printf("test_shm OK\n");
  return 0;
}
