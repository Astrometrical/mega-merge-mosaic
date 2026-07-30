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
}  // namespace mmm
#else
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
}  // namespace mmm
#endif
