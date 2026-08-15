#pragma once
// mmm_os.h -- minimal OS handle/IO seam so the transport code is written once.
// POSIX uses int fds; Windows uses HANDLEs. Everything else stays portable.
//
// The abstraction is deliberately tiny: a handle type (`os_handle`), an invalid
// sentinel (`os_invalid_handle`), and blocking `os_read`/`os_write`/`os_close`
// primitives. Higher-level framing (mmm_protocol) and the spawn/pipe/wait
// driver (mmm_host) are written against these so the wire format and control
// flow are shared across platforms; only the raw byte IO differs.
#include <cstddef>

#ifdef _WIN32
  #ifndef WIN32_LEAN_AND_MEAN
    #define WIN32_LEAN_AND_MEAN
  #endif
  #include <windows.h>
namespace mmm {
/// Platform handle type: a Win32 `HANDLE`.
using os_handle = HANDLE;
/// Sentinel for an unset/closed handle (matches Win32 conventions).
inline const os_handle os_invalid_handle = INVALID_HANDLE_VALUE;

/// Blocking read of up to `n` bytes; returns bytes read, 0 on EOF (peer
/// closed the pipe), or -1 on a hard error. A closed peer surfaces as
/// `ERROR_BROKEN_PIPE`, which we map to EOF to match POSIX `read()` returning
/// 0 at end of stream.
inline long os_read(os_handle h, void* buf, size_t n) {
    DWORD got = 0;
    if (!ReadFile(h, buf, (DWORD)n, &got, nullptr)) {
        if (GetLastError() == ERROR_BROKEN_PIPE) return 0;  // peer closed == EOF
        return -1;
    }
    return (long)got;
}

/// Blocking write of up to `n` bytes; returns bytes written or -1 on error.
/// A broken pipe (`ERROR_BROKEN_PIPE`/`ERROR_NO_DATA`) surfaces as -1, which
/// the caller maps to the same clean-error path POSIX takes on EPIPE.
inline long os_write(os_handle h, const void* buf, size_t n) {
    DWORD put = 0;
    if (!WriteFile(h, buf, (DWORD)n, &put, nullptr)) {
        return -1;  // includes ERROR_BROKEN_PIPE/ERROR_NO_DATA (== EPIPE)
    }
    return (long)put;
}

/// Close a handle, tolerating the invalid/null sentinels.
inline void os_close(os_handle h) {
    if (h && h != os_invalid_handle) CloseHandle(h);
}

/// Waits up to `timeout_ms` for `h` (a pipe read end) to have data (or EOF /
/// error) available for a non-blocking-ish `os_read`. Returns 1 when a read
/// would make progress, 0 on timeout. Anonymous pipe handles cannot be waited
/// on directly on Windows, so this polls `PeekNamedPipe` (which does work on
/// anonymous pipes) in short sleeps; a broken pipe reports 1 so the caller's
/// read observes the EOF.
inline int os_wait_readable(os_handle h, int timeout_ms) {
    for (int waited = 0;;) {
        DWORD avail = 0;
        if (!PeekNamedPipe(h, nullptr, 0, nullptr, &avail, nullptr))
            return 1;  // broken/closed pipe: let the read surface EOF/error
        if (avail > 0) return 1;
        if (waited >= timeout_ms) return 0;
        DWORD slice = (DWORD)((timeout_ms - waited) < 10 ? (timeout_ms - waited) : 10);
        Sleep(slice);
        waited += (int)slice;
    }
}

/// Two-pipe variant of `os_wait_readable`: waits up to `timeout_ms` for
/// either read end to have data (or EOF/error) available. Returns a bitmask
/// -- bit 0 set when `h1` is readable, bit 1 for `h2` -- or 0 on timeout.
/// Either handle may be `os_invalid_handle` to be ignored (both invalid
/// returns 0 immediately). Same `PeekNamedPipe`-poll mechanism as the
/// single-pipe wait; a broken pipe reports readable so the caller's read
/// observes the EOF.
inline int os_wait_readable2(os_handle h1, os_handle h2, int timeout_ms) {
    if (h1 == os_invalid_handle && h2 == os_invalid_handle) return 0;
    for (int waited = 0;;) {
        int mask = 0;
        if (h1 != os_invalid_handle) {
            DWORD avail = 0;
            if (!PeekNamedPipe(h1, nullptr, 0, nullptr, &avail, nullptr) || avail > 0) mask |= 1;
        }
        if (h2 != os_invalid_handle) {
            DWORD avail = 0;
            if (!PeekNamedPipe(h2, nullptr, 0, nullptr, &avail, nullptr) || avail > 0) mask |= 2;
        }
        if (mask != 0) return mask;
        if (waited >= timeout_ms) return 0;
        DWORD slice = (DWORD)((timeout_ms - waited) < 10 ? (timeout_ms - waited) : 10);
        Sleep(slice);
        waited += (int)slice;
    }
}
}  // namespace mmm
#else
  #include <cerrno>
  #include <poll.h>
  #include <unistd.h>
namespace mmm {
/// Platform handle type: a POSIX file descriptor.
using os_handle = int;
/// Sentinel for an unset/closed descriptor.
inline constexpr os_handle os_invalid_handle = -1;

/// Blocking read; returns bytes read, 0 on EOF, or -1 on error (errno set).
inline long os_read(os_handle h, void* buf, size_t n) { return (long)::read(h, buf, n); }
/// Blocking write; returns bytes written or -1 on error (errno set).
inline long os_write(os_handle h, const void* buf, size_t n) { return (long)::write(h, buf, n); }
/// Close a descriptor, tolerating the invalid sentinel.
inline void os_close(os_handle h) { if (h >= 0) ::close(h); }

/// Waits up to `timeout_ms` for `h` to be readable (data, EOF, or error).
/// Returns 1 when a read would make progress, 0 on timeout. A `poll()` error
/// (other than EINTR, which retries) reports 1 so the caller's read surfaces
/// the real errno.
inline int os_wait_readable(os_handle h, int timeout_ms) {
    struct pollfd pfd;
    pfd.fd = h;
    pfd.events = POLLIN;
    pfd.revents = 0;
    for (;;) {
        int r = ::poll(&pfd, 1, timeout_ms);
        if (r > 0) return 1;   // readable, EOF (POLLHUP), or error (POLLERR)
        if (r == 0) return 0;  // timeout
        if (errno != EINTR) return 1;  // let the read surface the error
    }
}

/// Two-pipe variant of `os_wait_readable`: waits up to `timeout_ms` for
/// either read end to have data (or EOF/error) available. Returns a bitmask
/// -- bit 0 set when `h1` is readable, bit 1 for `h2` -- or 0 on timeout.
/// Either handle may be `os_invalid_handle` to be ignored (both invalid
/// returns 0 immediately). A `poll()` error (other than EINTR, which
/// retries) reports both live handles readable so the caller's reads
/// surface the real errno.
inline int os_wait_readable2(os_handle h1, os_handle h2, int timeout_ms) {
    struct pollfd pfds[2];
    int n = 0;
    int idx1 = -1, idx2 = -1;
    if (h1 != os_invalid_handle) {
        pfds[n].fd = h1;
        pfds[n].events = POLLIN;
        pfds[n].revents = 0;
        idx1 = n++;
    }
    if (h2 != os_invalid_handle) {
        pfds[n].fd = h2;
        pfds[n].events = POLLIN;
        pfds[n].revents = 0;
        idx2 = n++;
    }
    if (n == 0) return 0;
    for (;;) {
        int r = ::poll(pfds, (nfds_t)n, timeout_ms);
        if (r > 0) {
            int mask = 0;
            if (idx1 >= 0 && pfds[idx1].revents != 0) mask |= 1;
            if (idx2 >= 0 && pfds[idx2].revents != 0) mask |= 2;
            return mask;
        }
        if (r == 0) return 0;          // timeout
        if (errno != EINTR) {          // let the reads surface the error
            return (idx1 >= 0 ? 1 : 0) | (idx2 >= 0 ? 2 : 0);
        }
    }
}
}  // namespace mmm
#endif
