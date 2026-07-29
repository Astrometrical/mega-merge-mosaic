#pragma once
// Named shared-memory segment + fixed slot layout, mirroring
// crates/mmm-core/src/ipc/shm.rs's `SlotLayout`/`ShmSegment`. No PCL
// headers here -- only the C++ stdlib + POSIX/Win32 -- so this compiles and
// tests without PixInsight installed.
//
// POSIX backs the segment with `shm_open`+`mmap`; Windows backs it with a
// pagefile-backed `CreateFileMappingW`+`MapViewOfFile`. The public interface
// is identical on both; only the private handle member differs.

#include <cstdint>
#include <string>

#include "mmm_os.h"

namespace mmm {

/// Describes how a shared-memory segment is carved into fixed-size slots:
/// `input_slots` slots (filled by the host, read by the worker) followed
/// by `output_slots` slots (filled by the worker, read by the host).
/// Matches PROTOCOL.md Section 7 and `crates/mmm-core/src/ipc/shm.rs`.
struct SlotLayout {
  uint64_t slot_bytes;
  uint32_t input_slots;
  uint32_t output_slots;

  uint64_t total_bytes() const {
    return slot_bytes * (uint64_t(input_slots) + output_slots);
  }
  uint64_t input_offset(uint32_t i) const { return slot_bytes * i; }
  uint64_t output_offset(uint32_t i) const {
    return slot_bytes * input_slots + slot_bytes * i;
  }
};

/// A named shared-memory segment mapped into this process.
///
/// POSIX: the creator (`create`) ftruncates + mmaps the segment and unlinks
/// the named object on destruction. Windows: the creator maps a pagefile-
/// backed `CreateFileMappingW` object; there is NO `shm_unlink` analog -- the
/// object is handle-refcounted by the kernel and vanishes when the last handle
/// (host + worker) closes, so the destructor only unmaps the view and closes
/// the mapping handle. A moved-from instance performs no unmap/close on
/// destruction.
class ShmSegment {
 public:
  /// Create a new shared-memory segment named `name`, sized
  /// `total_bytes`, and map it into this process. Throws
  /// `std::runtime_error` (with `strerror(errno)`) on failure.
  static ShmSegment create(const std::string& name, uint64_t total_bytes);

  ~ShmSegment();

  ShmSegment(ShmSegment&& other) noexcept;
  ShmSegment& operator=(ShmSegment&& other) noexcept;

  ShmSegment(const ShmSegment&) = delete;
  ShmSegment& operator=(const ShmSegment&) = delete;

  /// Mapped base pointer.
  uint8_t* base() const { return base_; }
  /// Size in bytes of the mapping.
  uint64_t size() const { return size_; }
  /// `base() + byte_offset` reinterpreted as `float*`. Asserts
  /// `byte_offset % 4 == 0` (4-byte f32 alignment, PROTOCOL.md Section 7).
  float* slot_floats(uint64_t byte_offset) const;
  /// The name this segment was created under.
  const std::string& name() const { return name_; }

 private:
  ShmSegment() = default;
  ShmSegment(std::string name, uint8_t* base, uint64_t size, bool is_creator);

  std::string name_;
  uint8_t* base_ = nullptr;
  uint64_t size_ = 0;
  bool is_creator_ = false;
#ifdef _WIN32
  // The file-mapping object handle. On Windows there is no shm_unlink; the
  // mapping lives as long as any handle to it is open (see class docs).
  os_handle hMap_ = nullptr;
#endif
};

}  // namespace mmm
