#include "../mmm_shm.h"
#include "test_util.h"
#include <cstring>
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
  std::printf("test_shm OK\n");
  return 0;
}
