#pragma once
// POSIX shared-memory segment + fixed slot layout, mirroring
// crates/mmm-core/src/ipc/shm.rs's `SlotLayout`/`ShmSegment`. No PCL
// headers here -- only the C++ stdlib + POSIX -- so this compiles and
// tests without PixInsight installed.

#include <cstdint>
#include <string>

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

/// A named POSIX shared-memory segment mapped into this process.
///
/// The creator (`create`) ftruncates + mmaps the segment and unlinks the
/// named object on destruction. A moved-from instance is left in a state
/// that performs no unmap/unlink on destruction.
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
  ShmSegment(std::string name, uint8_t* base, uint64_t size, bool is_creator);

  std::string name_;
  uint8_t* base_ = nullptr;
  uint64_t size_ = 0;
  bool is_creator_ = false;
};

}  // namespace mmm
