// MmmVersion.h -- single source for the module's human-readable version.
// mmm.cpp derives its PCL_MODULE_VERSION components from the same numbers;
// keep both in sync when bumping. MMM_VERSION_STRING must also stay in
// exact sync with the workspace Cargo.toml version and the host library's
// kExpectedWorkerVersion (mmm_protocol.h) -- the module/worker version
// handshake compares these strings for equality (enforced by
// crates/mmm-ipc-worker/tests/version_sync.rs).

#ifndef __MmmVersion_h
#define __MmmVersion_h

#define MMM_VERSION_MAJOR     1
#define MMM_VERSION_MINOR     3
#define MMM_VERSION_REVISION  1
#define MMM_VERSION_BUILD     1

#define MMM_VERSION_STRING    "1.3.1"

#endif   // __MmmVersion_h
