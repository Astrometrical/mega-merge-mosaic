#include "mmm_host.h"

#include "mmm_os.h"

#ifndef _WIN32
#include <spawn.h>
#include <sys/wait.h>
#include <unistd.h>
#include <csignal>
#endif

#include <cerrno>
#include <cstdio>
#include <cstring>
#include <mutex>
#include <string>
#include <vector>

#ifndef _WIN32
extern char** environ;
#endif

namespace mmm {

namespace {

#ifdef _WIN32
// Formats the current GetLastError() as a human string for diagnostics.
std::string last_error() {
  DWORD e = GetLastError();
  char* msg = nullptr;
  DWORD n = FormatMessageA(
      FORMAT_MESSAGE_ALLOCATE_BUFFER | FORMAT_MESSAGE_FROM_SYSTEM |
          FORMAT_MESSAGE_IGNORE_INSERTS,
      nullptr, e, 0, reinterpret_cast<char*>(&msg), 0, nullptr);
  std::string out = "os error " + std::to_string(e);
  if (n && msg) {
    out += ": ";
    out.append(msg, n);
    LocalFree(msg);
  }
  return out;
}

// Widen a UTF-8 narrow string to UTF-16 for the Win32 *W APIs.
std::wstring widen(const std::string& s) {
  if (s.empty()) return std::wstring();
  int n = MultiByteToWideChar(CP_UTF8, 0, s.data(), static_cast<int>(s.size()), nullptr, 0);
  std::wstring w(static_cast<size_t>(n), L'\0');
  MultiByteToWideChar(CP_UTF8, 0, s.data(), static_cast<int>(s.size()), w.data(), n);
  return w;
}

// Appends `arg` to `cmd` using the standard Win32 command-line quoting rules
// (CommandLineToArgvW's inverse): quote only when the arg is empty or contains
// whitespace/quotes, doubling backslashes that precede a quote or the closing
// quote. Keeps simple args (e.g. "/c", "--probe-frame") unquoted.
void append_win_arg(std::wstring& cmd, const std::wstring& arg) {
  bool need_quote = arg.empty() || arg.find_first_of(L" \t\n\v\"") != std::wstring::npos;
  if (!need_quote) {
    cmd += arg;
    return;
  }
  cmd += L'"';
  for (size_t i = 0; i < arg.size();) {
    size_t backslashes = 0;
    while (i < arg.size() && arg[i] == L'\\') {
      ++i;
      ++backslashes;
    }
    if (i == arg.size()) {
      cmd.append(backslashes * 2, L'\\');  // escape trailing backslashes before closing quote
      break;
    } else if (arg[i] == L'"') {
      cmd.append(backslashes * 2 + 1, L'\\');  // escape backslashes + the quote
      cmd += L'"';
      ++i;
    } else {
      cmd.append(backslashes, L'\\');
      cmd += arg[i];
      ++i;
    }
  }
  cmd += L'"';
}

// Builds a mutable UTF-16 command line for CreateProcessW (which may modify the
// buffer in place, so callers pass `cmd.data()`):
//   "<worker>" <arg0> <arg1> ... [<extra_flag>]
// The worker exe is ALWAYS quoted so a spaced install path
// (e.g. C:\Program Files\PixInsight\bin\mmm-ipc-worker.exe) resolves correctly
// and the `C:\Program.exe` local-hijack misparse is avoided. Additional args
// come from HostConfig::worker_args (production leaves it empty; tests use it to
// spawn a real quick-exit process such as cmd.exe /c exit 1). A non-empty
// `extra_flag` (e.g. "--probe-frame", "--probe-panels") is appended last.
std::wstring build_command_line_w(const std::string& worker_path,
                                  const std::vector<std::string>& args,
                                  const std::string& extra_flag) {
  std::wstring cmd;
  cmd += L'"';
  cmd += widen(worker_path);
  cmd += L'"';
  for (const std::string& a : args) {
    cmd += L' ';
    append_win_arg(cmd, widen(a));
  }
  if (!extra_flag.empty()) {
    cmd += L' ';
    append_win_arg(cmd, widen(extra_flag));
  }
  return cmd;
}
#endif

// Ignore SIGPIPE process-wide, once, so a write to the stdin of a worker that
// has just died (its read end fully closed) fails with EPIPE instead of
// delivering the default-fatal SIGPIPE that would terminate the whole host.
// In the real PixInsight module a mistimed worker crash must NOT take down
// PixInsight itself -- that is the isolation guarantee of spec §9/§18 ("worker
// crash -> clean error, host keeps running"). With SIGPIPE ignored the write
// returns -1/EPIPE, which `full_write_fd` turns into a clean `HostError` (and
// which `cancel()` deliberately swallows). These are pipe `write()`s, so
// `MSG_NOSIGNAL`/`send()` do not apply; process-wide `SIG_IGN` is the correct,
// standard mechanism for a process doing pipe IO. `std::call_once` keeps it
// idempotent so repeated `Host` construction never thrashes the disposition.
//
// Windows has no SIGPIPE: a write to a broken pipe fails with
// ERROR_BROKEN_PIPE (surfaced by `os_write` as -1), never a fatal signal, so
// this is a no-op there.
void ignore_sigpipe_once() {
#ifndef _WIN32
  static std::once_flag flag;
  std::call_once(flag, [] { ::signal(SIGPIPE, SIG_IGN); });
#endif
}

// Writes all `n` bytes of `buf` to `fd`, looping over short writes and
// retrying on EINTR (POSIX only). Throws on a hard error (including a broken
// pipe, which `os_write` reports as -1).
void full_write_fd(os_handle fd, const uint8_t* buf, size_t n) {
  size_t total = 0;
  while (total < n) {
    long w = os_write(fd, buf + total, n - total);
    if (w < 0) {
#ifndef _WIN32
      if (errno == EINTR) continue;
      throw HostError(std::string("write to worker: ") + std::strerror(errno));
#else
      throw HostError("write to worker: broken pipe or write error");
#endif
    }
    total += static_cast<size_t>(w);
  }
}

// A pipe pair; [0] = read end, [1] = write end.
struct Pipe {
  os_handle fd[2] = {os_invalid_handle, os_invalid_handle};
  Pipe() {
#ifdef _WIN32
    // Create both ends inheritable; the caller marks the parent-retained end
    // non-inheritable via SetHandleInformation before spawning, so only the
    // child-side end is inherited (essential for correct EOF propagation).
    SECURITY_ATTRIBUTES sa;
    sa.nLength = sizeof(sa);
    sa.bInheritHandle = TRUE;
    sa.lpSecurityDescriptor = nullptr;
    if (!CreatePipe(&fd[0], &fd[1], &sa, 0)) {
      throw HostError(std::string("CreatePipe failed: ") + last_error());
    }
#else
    if (::pipe(fd) != 0) {
      throw HostError(std::string("pipe: ") + std::strerror(errno));
    }
#endif
  }
  void close_read() {
    if (fd[0] != os_invalid_handle) {
      os_close(fd[0]);
      fd[0] = os_invalid_handle;
    }
  }
  void close_write() {
    if (fd[1] != os_invalid_handle) {
      os_close(fd[1]);
      fd[1] = os_invalid_handle;
    }
  }
  ~Pipe() {
    close_read();
    close_write();
  }
};

#ifndef _WIN32
// Spawns `path` with `argv`, redirecting the child's stdin to `child_stdin_rd`,
// stdout to `child_stdout_wr`, and (when `child_stderr_wr >= 0`) stderr to
// `child_stderr_wr`; stderr is inherited otherwise. The raw pipe fds -- the
// dup2 sources and every parent-retained end in `parent_ends` -- are closed in
// the child after the dup2s so no descriptor leaks across exec.
pid_t spawn_worker(const std::string& path, char* const argv[], int child_stdin_rd,
                   int child_stdout_wr, int child_stderr_wr,
                   const std::vector<int>& parent_ends) {
  posix_spawn_file_actions_t fa;
  posix_spawn_file_actions_init(&fa);
  posix_spawn_file_actions_adddup2(&fa, child_stdin_rd, STDIN_FILENO);
  posix_spawn_file_actions_adddup2(&fa, child_stdout_wr, STDOUT_FILENO);
  if (child_stderr_wr >= 0) posix_spawn_file_actions_adddup2(&fa, child_stderr_wr, STDERR_FILENO);
  posix_spawn_file_actions_addclose(&fa, child_stdin_rd);
  posix_spawn_file_actions_addclose(&fa, child_stdout_wr);
  if (child_stderr_wr >= 0) posix_spawn_file_actions_addclose(&fa, child_stderr_wr);
  for (int fd : parent_ends) {
    if (fd >= 0) posix_spawn_file_actions_addclose(&fa, fd);
  }

  pid_t pid = -1;
  int rc = posix_spawn(&pid, path.c_str(), &fa, nullptr, argv, environ);
  posix_spawn_file_actions_destroy(&fa);
  if (rc != 0) {
    throw HostError(std::string("posix_spawn ") + path + ": " + std::strerror(rc));
  }
  return pid;
}
#else
// Windows spawn: CreateProcessW with STARTF_USESTDHANDLES wiring the child's
// stdin to `child_stdin_rd`, stdout to `child_stdout_wr`, and stderr to
// `child_stderr_wr` when given (host stderr inherited otherwise).
// `bInheritHandles = TRUE` is required for STARTF_USESTDHANDLES to take
// effect; the caller must have already made the parent-retained pipe ends
// non-inheritable so the child does not keep them open (which would prevent
// EOF). Returns the child process HANDLE (caller closes it after reaping).
HANDLE spawn_worker_win(const std::string& path, const std::vector<std::string>& args,
                        const std::string& extra_flag, os_handle child_stdin_rd,
                        os_handle child_stdout_wr,
                        os_handle child_stderr_wr = os_invalid_handle) {
  STARTUPINFOW si;
  ZeroMemory(&si, sizeof si);
  si.cb = sizeof si;
  si.dwFlags = STARTF_USESTDHANDLES;
  si.hStdInput = child_stdin_rd;    // child reads its stdin
  si.hStdOutput = child_stdout_wr;  // child writes its stdout
  si.hStdError = (child_stderr_wr != os_invalid_handle)
                     ? child_stderr_wr                    // capture into a pipe
                     : GetStdHandle(STD_ERROR_HANDLE);    // inherit host stderr

  PROCESS_INFORMATION pi;
  ZeroMemory(&pi, sizeof pi);

  std::wstring cmd = build_command_line_w(path, args, extra_flag);
  // CREATE_NO_WINDOW: the worker is a console app but the host may be a GUI
  // process (PixInsight); without this flag Windows pops a visible console
  // window for every worker launch. The child still gets an (invisible)
  // console, so its std handles keep working.
  BOOL ok = CreateProcessW(nullptr, cmd.data(), nullptr, nullptr,
                           TRUE /*inherit handles*/, CREATE_NO_WINDOW, nullptr, nullptr, &si, &pi);
  if (!ok) {
    throw HostError("CreateProcessW(" + path + ") failed: " + last_error());
  }
  CloseHandle(pi.hThread);
  return pi.hProcess;
}
#endif

// Spawns `worker_path <flag>`, writes `payload` to its stdin (the probe modes
// read stdin to EOF before writing anything, so a full blocking write cannot
// deadlock against a full output pipe), closes stdin, then drains stdout AND
// stderr concurrently with the same pumped wait loop Host::run uses
// (os_wait_readable2 + prog->on_idle() roughly every Host::kIdleWaitMs).
// Reaps the child on every path. Returns captured stdout on exit 0. Throws
// HostCancelled when prog->want_cancel() fires during the drain (child killed
// first); throws HostError on a nonzero exit, with the trimmed captured
// stderr in the message (fallback text when stderr is empty).
std::string run_probe_process(const std::string& worker_path, const std::string& flag,
                              const std::string& payload, ProgressCallback* prog) {
  Pipe in_pipe;   // host writes -> worker stdin
  Pipe out_pipe;  // worker stdout -> host
  Pipe err_pipe;  // worker stderr -> host (captured for error messages)

#ifdef _WIN32
  SetHandleInformation(in_pipe.fd[1], HANDLE_FLAG_INHERIT, 0);
  SetHandleInformation(out_pipe.fd[0], HANDLE_FLAG_INHERIT, 0);
  SetHandleInformation(err_pipe.fd[0], HANDLE_FLAG_INHERIT, 0);
  HANDLE child = spawn_worker_win(worker_path, /*args=*/{}, flag, in_pipe.fd[0], out_pipe.fd[1],
                                  err_pipe.fd[1]);
#else
  std::string arg0 = worker_path;
  std::string arg1 = flag;
  char* argv[] = {arg0.data(), arg1.data(), nullptr};
  pid_t pid = spawn_worker(worker_path, argv, in_pipe.fd[0], out_pipe.fd[1], err_pipe.fd[1],
                           {in_pipe.fd[1], out_pipe.fd[0], err_pipe.fd[0]});
#endif
  in_pipe.close_read();
  out_pipe.close_write();
  err_pipe.close_write();

  // Mirrors probe_frame's historical fault-isolation guard: any throw between
  // spawn and reap must still kill+reap the child so a failing probe never
  // leaks a zombie; `reaped` ensures the happy path waits exactly once.
  bool reaped = false;
#ifndef _WIN32
  int status = 0;
#else
  DWORD code = 0;
#endif
  auto kill_and_reap = [&]() {
#ifdef _WIN32
    TerminateProcess(child, 1);
    WaitForSingleObject(child, INFINITE);
    CloseHandle(child);
#else
    ::kill(pid, SIGKILL);
    ::waitpid(pid, &status, 0);
#endif
    reaped = true;
  };

  std::string out;
  std::string err;
  try {
    full_write_fd(in_pipe.fd[1], reinterpret_cast<const uint8_t*>(payload.data()), payload.size());
    in_pipe.close_write();

    bool out_open = true;
    bool err_open = true;
    char buf[4096];
    while (out_open || err_open) {
      int r = os_wait_readable2(out_open ? out_pipe.fd[0] : os_invalid_handle,
                                err_open ? err_pipe.fd[0] : os_invalid_handle, Host::kIdleWaitMs);
      if (r == 0) {
        if (prog != nullptr) {
          prog->on_idle();
          if (prog->want_cancel()) {
            kill_and_reap();
            throw HostCancelled{};
          }
        }
        continue;
      }
      if ((r & 1) && out_open) {
        long n = os_read(out_pipe.fd[0], buf, sizeof buf);
        if (n > 0) {
          out.append(buf, static_cast<size_t>(n));
        } else {
#ifndef _WIN32
          if (!(n < 0 && errno == EINTR)) out_open = false;
#else
          out_open = false;
#endif
        }
      }
      if ((r & 2) && err_open) {
        long n = os_read(err_pipe.fd[0], buf, sizeof buf);
        if (n > 0) {
          err.append(buf, static_cast<size_t>(n));
        } else {
#ifndef _WIN32
          if (!(n < 0 && errno == EINTR)) err_open = false;
#else
          err_open = false;
#endif
        }
      }
    }

#ifdef _WIN32
    WaitForSingleObject(child, INFINITE);
    GetExitCodeProcess(child, &code);
    CloseHandle(child);
    reaped = true;
    const bool failed = (code != 0);
#else
    ::waitpid(pid, &status, 0);
    reaped = true;
    const bool failed = !WIFEXITED(status) || WEXITSTATUS(status) != 0;
#endif
    if (failed) {
      while (!err.empty() &&
             (err.back() == '\n' || err.back() == '\r' || err.back() == ' ')) {
        err.pop_back();
      }
      throw HostError("worker " + flag + " failed: " +
                      (err.empty() ? std::string("exited abnormally") : err));
    }
  } catch (...) {
    if (!reaped) kill_and_reap();
    throw;
  }
  return out;
}

}  // namespace

Host::Host(HostConfig cfg, PanelSource& src, OutputCollector& out, ProgressCallback* prog)
    : cfg_(std::move(cfg)), src_(src), out_(out), prog_(prog) {
  // Isolation guarantee (spec §9/§18): a dying worker must never signal the
  // host to death via a pipe write. Idempotent; see ignore_sigpipe_once.
  ignore_sigpipe_once();
}

void Host::write_framed_locked(const std::vector<uint8_t>& framed) {
  std::lock_guard<std::mutex> lk(stdin_mutex_);
  if (stdin_fd_ == os_invalid_handle) return;  // worker gone; nothing to write.
  full_write_fd(stdin_fd_, framed.data(), framed.size());
}

void Host::cancel() {
  cancel_requested_.store(true);
  std::lock_guard<std::mutex> lk(stdin_mutex_);
  if (stdin_fd_ == os_invalid_handle) return;  // run() not started or finished.
  std::vector<uint8_t> framed = encode_cancel();
  // Best-effort: a worker that already exited yields EPIPE here; that's fine,
  // the reader loop will observe EOF and unwind. Swallow the write error.
  try {
    full_write_fd(stdin_fd_, framed.data(), framed.size());
  } catch (const std::exception&) {
  }
}

void Host::run() {
  // Parse the geometry the Init body promises the worker, up front: the serve
  // loop validates every worker-supplied wire value (Begin dimensions,
  // OutputBand ranges, BandRequest ranges) against it BEFORE the value
  // parameterizes any memcpy. Without these checks a misbehaving worker --
  // version skew, memory corruption, a plain bug -- would make this host
  // write past the collector's image planes or past the shm mapping: silent
  // heap corruption inside the embedding application that detonates long
  // after the run (PROTOCOL.md Section 13).
  uint64_t canvas_w = 0, canvas_h = 0, canvas_ch = 0;
  struct PanelDims {
    uint64_t w, h, ch;
  };
  std::vector<PanelDims> panel_dims;
  try {
    const nlohmann::json& body = cfg_.init.at("Init");
    const nlohmann::json& canvas = body.at("canvas");
    canvas_w = canvas.at(0).get<uint64_t>();
    canvas_h = canvas.at(1).get<uint64_t>();
    canvas_ch = canvas.at(2).get<uint64_t>();
    for (const auto& p : body.at("panels")) {
      panel_dims.push_back({p.at("width").get<uint64_t>(), p.at("height").get<uint64_t>(),
                            p.at("channels").get<uint64_t>()});
    }
  } catch (const nlohmann::json::exception& e) {
    throw HostError(std::string("Init body is missing canvas/panels geometry: ") + e.what());
  }

  // RAII: the segment is released when `shm` leaves scope on ANY path (POSIX
  // unlinks; Windows drops its handle -- the mapping vanishes on last close).
  ShmSegment shm = ShmSegment::create(cfg_.shm_name, cfg_.layout.total_bytes());

  Pipe in_pipe;   // host writes in_pipe[1] -> worker stdin (in_pipe[0])
  Pipe out_pipe;  // worker stdout (out_pipe[1]) -> host reads out_pipe[0]

#ifdef _WIN32
  // Only the child-inherited ends stay inheritable; make the host-retained
  // ends non-inheritable so the child does not hold copies (breaks EOF).
  SetHandleInformation(in_pipe.fd[1], HANDLE_FLAG_INHERIT, 0);
  SetHandleInformation(out_pipe.fd[0], HANDLE_FLAG_INHERIT, 0);
  HANDLE child = spawn_worker_win(cfg_.worker_path, cfg_.worker_args, /*extra_flag=*/"",
                                  in_pipe.fd[0], out_pipe.fd[1]);
#else
  // argv[0] is the exact exe path (no quoting needed for exec); any extra
  // worker_args follow it. Storage must outlive posix_spawn.
  std::vector<std::string> argv_store;
  argv_store.reserve(1 + cfg_.worker_args.size());
  argv_store.push_back(cfg_.worker_path);
  for (const std::string& a : cfg_.worker_args) argv_store.push_back(a);
  std::vector<char*> argv;
  argv.reserve(argv_store.size() + 1);
  for (std::string& s : argv_store) argv.push_back(s.data());
  argv.push_back(nullptr);
  pid_t pid = spawn_worker(cfg_.worker_path, argv.data(), in_pipe.fd[0], out_pipe.fd[1],
                           /*child_stderr_wr=*/-1, {in_pipe.fd[1], out_pipe.fd[0]});
#endif

  // The child holds its own copies of stdin-rd / stdout-wr now; close our
  // copies of the child ends so EOF propagates correctly.
  in_pipe.close_read();
  out_pipe.close_write();

  {
    std::lock_guard<std::mutex> lk(stdin_mutex_);
    stdin_fd_ = in_pipe.fd[1];
  }
  os_handle stdout_fd = out_pipe.fd[0];

  bool saw_done = false;
  try {
    // Stamp the version handshake (PROTOCOL.md Section 10) into the Init:
    // the transport layer owns protocol/version concerns, so callers cannot
    // ship a skewed pair by forgetting (or hardcoding) either field.
    cfg_.init["Init"]["protocol_version"] = kProtocolVersion;
    cfg_.init["Init"]["worker_version"] = kExpectedWorkerVersion;

    // Send Init (fully framed by encode_init).
    write_framed_locked(encode_init(cfg_.init));

    // If cancel() was requested before the worker existed, deliver it now.
    if (cancel_requested_.load()) {
      write_framed_locked(encode_cancel());
    }

    for (;;) {
      // While the pipe is quiet, keep the observer's on_idle() ticking (a GUI
      // host pumps its event queue there) instead of parking in a blind
      // blocking read. When frames are flowing, os_wait_readable returns
      // immediately with data available, so the serve loop is not slowed.
      if (prog_ != nullptr) {
        while (os_wait_readable(stdout_fd, kIdleWaitMs) == 0) {
          prog_->on_idle();
        }
      }
      WorkerFrame wf;
      bool got = read_worker_frame(stdout_fd, wf);
      if (!got) {
        // Clean EOF at a frame boundary: the worker closed stdout. If we
        // have not seen Done, the worker exited early (crash/abort).
        break;
      }
      switch (wf.tag) {
        case WorkerTag::BandRequest: {
          const BandRequest& r = wf.band_request;
          // Host memory safety first: a slot id outside the pool, or a band
          // bigger than one slot, would make the PanelSource write outside
          // the shm mapping. These are protocol violations -- kill the run.
          if (r.slot_id >= cfg_.layout.input_slots) {
            throw HostError("worker sent BandRequest with input slot " +
                            std::to_string(r.slot_id) + " out of range (input_slots=" +
                            std::to_string(cfg_.layout.input_slots) + ")");
          }
          bool in_range = r.panel_id < panel_dims.size() && r.y0 <= r.y1;
          if (in_range) {
            const PanelDims& pd = panel_dims[r.panel_id];
            const uint64_t rows = r.y1 - r.y0;
            const uint64_t row_bytes = pd.w * pd.ch * 4;
            if (row_bytes != 0 && rows > cfg_.layout.slot_bytes / row_bytes) {
              throw HostError("worker sent BandRequest for " + std::to_string(rows) +
                              " rows of panel " + std::to_string(r.panel_id) + " (" +
                              std::to_string(rows * row_bytes) + " bytes), which exceeds the slot size " +
                              std::to_string(cfg_.layout.slot_bytes));
            }
            // A row range beyond the panel is not a memory hazard here (the
            // PanelSource checks its own bounds), but refuse it up front so
            // the source never sees it; the worker unwinds via status=1.
            in_range = row_bytes != 0 && r.y1 <= pd.h;
          }
          bool ok = false;
          if (in_range) {
            float* dst = shm.slot_floats(cfg_.layout.input_offset(r.slot_id));
            ok = src_.fill_band(r.panel_id, r.y0, r.y1, dst);
          }
          BandReply reply{r.request_id, r.slot_id, static_cast<uint8_t>(ok ? 0 : 1)};
          std::vector<uint8_t> payload = encode_band_reply(reply);
          {
            std::lock_guard<std::mutex> lk(stdin_mutex_);
            if (stdin_fd_ != os_invalid_handle) {
              write_frame_raw(stdin_fd_, static_cast<uint8_t>(HostTag::BandReply), payload.data(),
                              static_cast<uint32_t>(payload.size()));
            }
          }
          break;
        }
        case WorkerTag::Begin: {
          if (begun_) {
            throw HostError("worker sent a duplicate Begin");
          }
          if (wf.begin_w == 0 || wf.begin_h == 0 || wf.begin_ch == 0) {
            throw HostError("worker sent Begin with an empty output geometry (" +
                            std::to_string(wf.begin_w) + "x" + std::to_string(wf.begin_h) + "x" +
                            std::to_string(wf.begin_ch) + ")");
          }
          // The collector narrows these to int (pcl::ImageWindow takes int
          // dimensions); anything beyond int32 would truncate.
          constexpr uint64_t kMaxDim = 0x7fffffff;
          if (wf.begin_w > kMaxDim || wf.begin_h > kMaxDim || wf.begin_ch > kMaxDim) {
            throw HostError("worker sent Begin with an output geometry too large for the "
                            "collector (" + std::to_string(wf.begin_w) + "x" +
                            std::to_string(wf.begin_h) + "x" + std::to_string(wf.begin_ch) + ")");
          }
          // The output is a crop of the Init canvas: never wider/taller than
          // it (when the canvas is known -- solved mode sends w = h = 0), and
          // always with its channel count.
          if (wf.begin_ch != canvas_ch) {
            throw HostError("worker sent Begin with " + std::to_string(wf.begin_ch) +
                            " channels; the Init canvas has " + std::to_string(canvas_ch));
          }
          if (canvas_w != 0 && canvas_h != 0 &&
              (wf.begin_w > canvas_w || wf.begin_h > canvas_h)) {
            throw HostError("worker sent Begin " + std::to_string(wf.begin_w) + "x" +
                            std::to_string(wf.begin_h) + ", which exceeds the " +
                            std::to_string(canvas_w) + "x" + std::to_string(canvas_h) +
                            " Init canvas");
          }
          begun_ = true;
          out_w_ = wf.begin_w;
          out_h_ = wf.begin_h;
          out_ch_ = wf.begin_ch;
          out_.begin(wf.begin_w, wf.begin_h, wf.begin_ch);
          break;
        }
        case WorkerTag::OutputBand: {
          const OutputBand& b = wf.output_band;
          // Every value here parameterizes the collector's memcpy into the
          // output image (and the read from the shm slot): validate all of
          // them against the Begin geometry before touching either.
          if (!begun_) {
            throw HostError("worker sent OutputBand before Begin");
          }
          if (b.slot_id >= cfg_.layout.output_slots) {
            throw HostError("worker sent OutputBand with output slot " +
                            std::to_string(b.slot_id) + " out of range (output_slots=" +
                            std::to_string(cfg_.layout.output_slots) + ")");
          }
          if (b.rows == 0 || b.y0 > out_h_ || b.rows > out_h_ - b.y0) {
            throw HostError("worker sent OutputBand rows [" + std::to_string(b.y0) + ", " +
                            std::to_string(b.y0) + "+" + std::to_string(b.rows) +
                            ") past the canvas height " + std::to_string(out_h_));
          }
          // out_w_/out_ch_ are Begin-validated (nonzero, <= int32 max), so
          // row_bytes is nonzero and this division form cannot overflow.
          const uint64_t row_bytes = out_w_ * out_ch_ * 4;
          if (b.rows > cfg_.layout.slot_bytes / row_bytes) {
            throw HostError("worker sent OutputBand of " + std::to_string(b.rows) + " rows (" +
                            std::to_string(b.rows * row_bytes) +
                            " bytes), which exceeds the slot size " +
                            std::to_string(cfg_.layout.slot_bytes));
          }
          const float* p = shm.slot_floats(cfg_.layout.output_offset(b.slot_id));
          out_.band(b.y0, b.rows, p, out_w_, out_ch_);
          write_framed_locked(encode_output_ack(b.request_id));
          break;
        }
        case WorkerTag::Progress: {
          if (prog_) prog_->on_progress(wf.progress_stage, wf.progress_done, wf.progress_total);
          break;
        }
        case WorkerTag::Done: {
          saw_done = true;
          break;
        }
        case WorkerTag::Error: {
          if (cancel_requested_.load()) {
            // Intentional stop: the worker acknowledged our Cancel.
            cancelled_.store(true);
            break;
          }
          throw HostError(std::string("worker error: ") + wf.error_message);
        }
        case WorkerTag::Invalid: {
          // Unreachable in practice: `read_worker_frame` only ever returns
          // true after setting `tag` to one of the six real worker tags
          // (any other byte on the wire throws before we get here). Guard
          // it anyway so a default-constructed-but-unfilled frame can never
          // silently fall through as a real message.
          throw HostError("internal error: WorkerFrame::tag left Invalid");
        }
      }
      if (saw_done || cancelled_.load()) break;
    }
  } catch (...) {
    // Fault isolation: reap the child (kill first in case it is wedged) so we
    // never hang, close our fds, let `shm` release via RAII, and surface a
    // clean HostError. The collector's partial buffer is the caller's to
    // discard -- we present no partial output as success.
#ifdef _WIN32
    TerminateProcess(child, 1);
    WaitForSingleObject(child, INFINITE);
    CloseHandle(child);
#else
    ::kill(pid, SIGKILL);
    int st = 0;
    ::waitpid(pid, &st, 0);
#endif
    os_close(stdout_fd);
    {
      std::lock_guard<std::mutex> lk(stdin_mutex_);
      if (stdin_fd_ != os_invalid_handle) os_close(stdin_fd_);
      stdin_fd_ = os_invalid_handle;
    }
    in_pipe.fd[1] = os_invalid_handle;   // already closed above; disarm Pipe dtor.
    out_pipe.fd[0] = os_invalid_handle;
    try {
      throw;
    } catch (const HostError&) {
      throw;
    } catch (const std::exception& e) {
      throw HostError(e.what());
    }
  }

  os_close(stdout_fd);
  out_pipe.fd[0] = os_invalid_handle;
  {
    std::lock_guard<std::mutex> lk(stdin_mutex_);
    if (stdin_fd_ != os_invalid_handle) os_close(stdin_fd_);
    stdin_fd_ = os_invalid_handle;
  }
  in_pipe.fd[1] = os_invalid_handle;

#ifdef _WIN32
  DWORD code = 0;
  WaitForSingleObject(child, INFINITE);
  GetExitCodeProcess(child, &code);
  CloseHandle(child);
#else
  int status = 0;
  ::waitpid(pid, &status, 0);
#endif

  if (cancelled_.load()) {
    // A cancelled run is a clean, intentional stop -- distinct from a crash
    // (which threw) and from a Done completion. The worker maps cancel to a
    // clean exit(0); we do not require Done and do not throw.
    return;
  }
  if (!saw_done) {
    throw HostError("worker exited before Done");
  }
#ifdef _WIN32
  // Windows has no WIFSIGNALED/exit-signal distinction; a TerminateProcess'd
  // or nonzero-exiting child is reported as an abnormal exit with its code.
  if (code != 0) {
    throw HostError(std::string("worker exited with status ") + std::to_string(code) +
                    " after Done");
  }
#else
  if (WIFSIGNALED(status)) {
    throw HostError(std::string("worker terminated by signal ") +
                    std::to_string(WTERMSIG(status)) + " after Done");
  }
  if (WIFEXITED(status) && WEXITSTATUS(status) != 0) {
    throw HostError(std::string("worker exited with status ") +
                    std::to_string(WEXITSTATUS(status)) + " after Done");
  }
#endif
}

void Host::probe_frame(const std::string& worker_path, const nlohmann::json& init_obj, uint64_t& w,
                       uint64_t& h, uint64_t& ch, ProgressCallback* prog) {
  // Same version stamping as Host::run() -- the probe is often the first
  // worker contact of a run, so a skewed module/worker pair fails here with
  // the worker's clear stderr message.
  nlohmann::json obj = init_obj;
  obj["protocol_version"] = kProtocolVersion;
  obj["worker_version"] = kExpectedWorkerVersion;
  const std::string out = run_probe_process(worker_path, "--probe-frame", obj.dump(), prog);
  unsigned long long pw = 0, ph = 0, pch = 0;
  if (std::sscanf(out.c_str(), "%llu %llu %llu", &pw, &ph, &pch) != 3) {
    throw HostError("probe-frame: could not parse \"w h ch\" from worker output: " + out);
  }
  w = pw;
  h = ph;
  ch = pch;
}

PanelProbeResult Host::probe_panels(const std::string& worker_path,
                                    const std::vector<std::string>& paths_utf8,
                                    const std::string& input_select, ProgressCallback* prog) {
  nlohmann::json req;
  req["worker_version"] = kExpectedWorkerVersion;  // as in Host::run()'s Init stamp
  req["paths"] = paths_utf8;
  req["input_select"] = input_select;
  const std::string out = run_probe_process(worker_path, "--probe-panels", req.dump(), prog);

  PanelProbeResult res;
  try {
    nlohmann::json reply = nlohmann::json::parse(out);
    for (const auto& p : reply.at("panels")) {
      ProbedPanel pp;
      pp.width = p.at("width").get<uint64_t>();
      pp.height = p.at("height").get<uint64_t>();
      pp.channels = p.at("channels").get<uint64_t>();
      res.panels.push_back(pp);
    }
    const auto& frame = reply.at("frame");
    if (!frame.is_null()) {
      res.has_frame = true;
      res.frame_w = frame.at(0).get<uint64_t>();
      res.frame_h = frame.at(1).get<uint64_t>();
      res.frame_ch = frame.at(2).get<uint64_t>();
    }
  } catch (const nlohmann::json::exception& e) {
    throw HostError(std::string("probe-panels: could not parse worker reply: ") + e.what());
  }
  return res;
}

}  // namespace mmm
