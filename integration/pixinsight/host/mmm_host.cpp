#include "mmm_host.h"

#include <spawn.h>
#include <sys/wait.h>
#include <unistd.h>

#include <cerrno>
#include <csignal>
#include <cstdio>
#include <cstring>
#include <mutex>
#include <vector>

extern char** environ;

namespace mmm {

namespace {

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
void ignore_sigpipe_once() {
  static std::once_flag flag;
  std::call_once(flag, [] { ::signal(SIGPIPE, SIG_IGN); });
}

// Writes all `n` bytes of `buf` to `fd`, looping over short writes and
// retrying on EINTR. Throws on a hard error.
void full_write_fd(int fd, const uint8_t* buf, size_t n) {
  size_t total = 0;
  while (total < n) {
    ssize_t w = ::write(fd, buf + total, n - total);
    if (w < 0) {
      if (errno == EINTR) continue;
      throw HostError(std::string("write to worker: ") + std::strerror(errno));
    }
    total += static_cast<size_t>(w);
  }
}

// A pipe pair; [0] = read end, [1] = write end.
struct Pipe {
  int fd[2] = {-1, -1};
  Pipe() {
    if (::pipe(fd) != 0) {
      throw HostError(std::string("pipe: ") + std::strerror(errno));
    }
  }
  void close_read() {
    if (fd[0] >= 0) {
      ::close(fd[0]);
      fd[0] = -1;
    }
  }
  void close_write() {
    if (fd[1] >= 0) {
      ::close(fd[1]);
      fd[1] = -1;
    }
  }
  ~Pipe() {
    close_read();
    close_write();
  }
};

// Spawns `path` with `argv`, redirecting the child's stdin to `child_stdin_rd`
// and stdout to `child_stdout_wr` (stderr inherited). The four raw pipe fds
// are closed in the child after the dup2s so no descriptor leaks across exec.
pid_t spawn_worker(const std::string& path, char* const argv[], int child_stdin_rd,
                   int child_stdout_wr, int close_a, int close_b) {
  posix_spawn_file_actions_t fa;
  posix_spawn_file_actions_init(&fa);
  posix_spawn_file_actions_adddup2(&fa, child_stdin_rd, STDIN_FILENO);
  posix_spawn_file_actions_adddup2(&fa, child_stdout_wr, STDOUT_FILENO);
  posix_spawn_file_actions_addclose(&fa, child_stdin_rd);
  posix_spawn_file_actions_addclose(&fa, child_stdout_wr);
  if (close_a >= 0) posix_spawn_file_actions_addclose(&fa, close_a);
  if (close_b >= 0) posix_spawn_file_actions_addclose(&fa, close_b);

  pid_t pid = -1;
  int rc = posix_spawn(&pid, path.c_str(), &fa, nullptr, argv, environ);
  posix_spawn_file_actions_destroy(&fa);
  if (rc != 0) {
    throw HostError(std::string("posix_spawn ") + path + ": " + std::strerror(rc));
  }
  return pid;
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
  if (stdin_fd_ < 0) return;  // worker gone; nothing to write.
  full_write_fd(stdin_fd_, framed.data(), framed.size());
}

void Host::cancel() {
  cancel_requested_.store(true);
  std::lock_guard<std::mutex> lk(stdin_mutex_);
  if (stdin_fd_ < 0) return;  // run() not started or already finished.
  std::vector<uint8_t> framed = encode_cancel();
  // Best-effort: a worker that already exited yields EPIPE here; that's fine,
  // the reader loop will observe EOF and unwind. Swallow the write error.
  try {
    full_write_fd(stdin_fd_, framed.data(), framed.size());
  } catch (const std::exception&) {
  }
}

void Host::run() {
  // RAII: the segment is unlinked when `shm` leaves scope on ANY path.
  ShmSegment shm = ShmSegment::create(cfg_.shm_name, cfg_.layout.total_bytes());

  Pipe in_pipe;   // host writes in_pipe[1] -> worker stdin (in_pipe[0])
  Pipe out_pipe;  // worker stdout (out_pipe[1]) -> host reads out_pipe[0]

  std::string arg0 = cfg_.worker_path;
  char* argv[] = {arg0.data(), nullptr};
  pid_t pid = spawn_worker(cfg_.worker_path, argv, in_pipe.fd[0], out_pipe.fd[1], in_pipe.fd[1],
                           out_pipe.fd[0]);

  // The child holds its own dup'd copies of stdin-rd / stdout-wr now; close
  // our copies of the child ends so EOF propagates correctly.
  in_pipe.close_read();
  out_pipe.close_write();

  {
    std::lock_guard<std::mutex> lk(stdin_mutex_);
    stdin_fd_ = in_pipe.fd[1];
  }
  int stdout_fd = out_pipe.fd[0];

  bool saw_done = false;
  try {
    // Send Init (fully framed by encode_init).
    write_framed_locked(encode_init(cfg_.init));

    // If cancel() was requested before the worker existed, deliver it now.
    if (cancel_requested_.load()) {
      write_framed_locked(encode_cancel());
    }

    for (;;) {
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
          float* dst = shm.slot_floats(cfg_.layout.input_offset(r.slot_id));
          bool ok = src_.fill_band(r.panel_id, r.y0, r.y1, dst);
          BandReply reply{r.request_id, r.slot_id, static_cast<uint8_t>(ok ? 0 : 1)};
          std::vector<uint8_t> payload = encode_band_reply(reply);
          {
            std::lock_guard<std::mutex> lk(stdin_mutex_);
            if (stdin_fd_ >= 0) {
              write_frame_raw(stdin_fd_, static_cast<uint8_t>(HostTag::BandReply), payload.data(),
                              static_cast<uint32_t>(payload.size()));
            }
          }
          break;
        }
        case WorkerTag::Begin: {
          out_w_ = wf.begin_w;
          out_ch_ = wf.begin_ch;
          out_.begin(wf.begin_w, wf.begin_h, wf.begin_ch);
          break;
        }
        case WorkerTag::OutputBand: {
          const OutputBand& b = wf.output_band;
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
      }
      if (saw_done || cancelled_.load()) break;
    }
  } catch (...) {
    // Fault isolation: reap the child (kill first in case it is wedged) so we
    // never hang, close our fds, let `shm` unlink via RAII, and surface a
    // clean HostError. The collector's partial buffer is the caller's to
    // discard -- we present no partial output as success.
    ::kill(pid, SIGKILL);
    int st = 0;
    ::waitpid(pid, &st, 0);
    ::close(stdout_fd);
    {
      std::lock_guard<std::mutex> lk(stdin_mutex_);
      if (stdin_fd_ >= 0) ::close(stdin_fd_);
      stdin_fd_ = -1;
    }
    in_pipe.fd[1] = -1;  // already closed above; disarm Pipe dtor.
    out_pipe.fd[0] = -1;
    try {
      throw;
    } catch (const HostError&) {
      throw;
    } catch (const std::exception& e) {
      throw HostError(e.what());
    }
  }

  ::close(stdout_fd);
  out_pipe.fd[0] = -1;
  {
    std::lock_guard<std::mutex> lk(stdin_mutex_);
    if (stdin_fd_ >= 0) ::close(stdin_fd_);
    stdin_fd_ = -1;
  }
  in_pipe.fd[1] = -1;

  int status = 0;
  ::waitpid(pid, &status, 0);

  if (cancelled_.load()) {
    // A cancelled run is a clean, intentional stop -- distinct from a crash
    // (which threw) and from a Done completion. The worker maps cancel to a
    // clean exit(0); we do not require Done and do not throw.
    return;
  }
  if (!saw_done) {
    throw HostError("worker exited before Done");
  }
  if (WIFSIGNALED(status)) {
    throw HostError(std::string("worker terminated by signal ") +
                    std::to_string(WTERMSIG(status)) + " after Done");
  }
  if (WIFEXITED(status) && WEXITSTATUS(status) != 0) {
    throw HostError(std::string("worker exited with status ") +
                    std::to_string(WEXITSTATUS(status)) + " after Done");
  }
}

void Host::probe_frame(const std::string& worker_path, const nlohmann::json& init_obj, uint64_t& w,
                       uint64_t& h, uint64_t& ch) {
  Pipe in_pipe;
  Pipe out_pipe;

  std::string arg0 = worker_path;
  std::string arg1 = "--probe-frame";
  char* argv[] = {arg0.data(), arg1.data(), nullptr};

  pid_t pid = spawn_worker(worker_path, argv, in_pipe.fd[0], out_pipe.fd[1], in_pipe.fd[1],
                           out_pipe.fd[0]);
  in_pipe.close_read();
  out_pipe.close_write();

  // From here on the child is spawned but not yet reaped: any throw (stdin
  // write EPIPE if the worker dies during startup, stdout drain, or parse)
  // must still kill+reap it, mirroring run()'s fault-isolation guard, so a
  // failing probe never leaks a zombie. `status`/`reaped` ensure the happy
  // path waits exactly once.
  int status = 0;
  bool reaped = false;
  try {
    // Write the probe JSON, then close stdin so the worker sees EOF.
    std::string payload = init_obj.dump();
    full_write_fd(in_pipe.fd[1], reinterpret_cast<const uint8_t*>(payload.data()), payload.size());
    in_pipe.close_write();

    // Read all of stdout.
    std::string out;
    {
      char buf[4096];
      for (;;) {
        ssize_t r = ::read(out_pipe.fd[0], buf, sizeof(buf));
        if (r < 0) {
          if (errno == EINTR) continue;
          break;
        }
        if (r == 0) break;
        out.append(buf, static_cast<size_t>(r));
      }
    }
    out_pipe.close_read();

    ::waitpid(pid, &status, 0);
    reaped = true;
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) {
      throw HostError("probe-frame: worker exited abnormally");
    }
    unsigned long long pw = 0, ph = 0, pch = 0;
    if (std::sscanf(out.c_str(), "%llu %llu %llu", &pw, &ph, &pch) != 3) {
      throw HostError("probe-frame: could not parse \"w h ch\" from worker output: " + out);
    }
    w = pw;
    h = ph;
    ch = pch;
  } catch (...) {
    if (!reaped) {
      ::kill(pid, SIGKILL);
      ::waitpid(pid, &status, 0);
    }
    throw;
  }
}

}  // namespace mmm
