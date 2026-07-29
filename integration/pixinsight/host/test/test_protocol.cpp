#include "../mmm_os.h"
#include "../mmm_protocol.h"
#include "test_util.h"
#include <vector>
using namespace mmm;

static void test_band_reply_bytes() {
  BandReply r{ /*request_id*/ 0x11223344u, /*slot_id*/ 0x55667788u, /*status*/ 1 };
  auto b = encode_band_reply(r);
  CHECK(b.size() == 9);
  // little-endian request_id, slot_id, then status
  CHECK(b[0]==0x44 && b[1]==0x33 && b[2]==0x22 && b[3]==0x11);
  CHECK(b[4]==0x88 && b[5]==0x77 && b[6]==0x66 && b[7]==0x55);
  CHECK(b[8]==1);
}

static void test_read_bandrequest_roundtrip() {
  // Build a 28-byte BandRequest payload framed as tag=1,len=28, write to a pipe,
  // read it back via read_worker_frame. Uses the OS-neutral pipe/IO seam so the
  // same round-trip runs on POSIX (pipe) and Windows (CreatePipe).
  os_handle rd = os_invalid_handle, wr = os_invalid_handle;
#ifdef _WIN32
  SECURITY_ATTRIBUTES sa; sa.nLength = sizeof(sa); sa.bInheritHandle = FALSE;
  sa.lpSecurityDescriptor = nullptr;
  CHECK(CreatePipe(&rd, &wr, &sa, 0));
#else
  int fds[2]; CHECK(pipe(fds)==0); rd = fds[0]; wr = fds[1];
#endif
  std::vector<uint8_t> frame;
  frame.push_back(1);                       // tag BandRequest
  uint32_t len=28; for(int i=0;i<4;i++) frame.push_back((len>>(8*i))&0xff);
  auto put32=[&](uint32_t v){ for(int i=0;i<4;i++) frame.push_back((v>>(8*i))&0xff); };
  auto put64=[&](uint64_t v){ for(int i=0;i<8;i++) frame.push_back((v>>(8*i))&0xff); };
  put32(7); put32(2); put64(16); put64(48); put32(3); // req,panel,y0,y1,slot
  CHECK(os_write(wr, frame.data(), frame.size()) == (long)frame.size());
  os_close(wr);
  WorkerFrame wf;
  CHECK(read_worker_frame(rd, wf));
  CHECK(wf.tag == WorkerTag::BandRequest);
  CHECK(wf.band_request.request_id==7 && wf.band_request.panel_id==2);
  CHECK(wf.band_request.y0==16 && wf.band_request.y1==48 && wf.band_request.slot_id==3);
  os_close(rd);
}

static void test_init_rejects_nonfinite() {
  nlohmann::json j; j["Init"]["params"]["feather_px"] = std::nan("");
  bool threw=false;
  try { encode_init(j); } catch(const std::exception&) { threw=true; }
  CHECK(threw);
}

int main(){ test_band_reply_bytes(); test_read_bandrequest_roundtrip(); test_init_rejects_nonfinite();
  std::printf("test_protocol OK\n"); return 0; }
