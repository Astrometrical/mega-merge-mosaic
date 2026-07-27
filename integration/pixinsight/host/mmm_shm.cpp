#include "mmm_shm.h"

#include <cassert>
#include <cerrno>
#include <cstring>
#include <fcntl.h>
#include <stdexcept>
#include <sys/mman.h>
#include <unistd.h>
#include <utility>

namespace mmm {

namespace {

[[noreturn]] void throw_errno(const std::string& what) {
  throw std::runtime_error(what + ": " + std::strerror(errno));
}

}  // namespace

ShmSegment::ShmSegment(std::string name, uint8_t* base, uint64_t size,
                        bool is_creator)
    : name_(std::move(name)), base_(base), size_(size), is_creator_(is_creator) {}

ShmSegment ShmSegment::create(const std::string& name, uint64_t total_bytes) {
  // Defensively unlink a stale segment left behind by a crashed prior run;
  // ENOENT (nothing to clean up) is expected and not an error.
  if (shm_unlink(name.c_str()) != 0 && errno != ENOENT) {
    throw_errno("shm_unlink(" + name + ") failed while clearing a stale segment");
  }

  int fd = shm_open(name.c_str(), O_CREAT | O_EXCL | O_RDWR, 0600);
  if (fd < 0) {
    throw_errno("shm_open(" + name + ") failed");
  }

  if (ftruncate(fd, static_cast<off_t>(total_bytes)) != 0) {
    int saved_errno = errno;
    close(fd);
    shm_unlink(name.c_str());
    errno = saved_errno;
    throw_errno("ftruncate(" + name + ", " + std::to_string(total_bytes) +
                " bytes) failed");
  }

  void* mapped = mmap(nullptr, total_bytes, PROT_READ | PROT_WRITE, MAP_SHARED,
                       fd, 0);
  if (mapped == MAP_FAILED) {
    int saved_errno = errno;
    close(fd);
    shm_unlink(name.c_str());
    errno = saved_errno;
    throw_errno("mmap(" + name + ") failed");
  }

  close(fd);

  return ShmSegment(name, static_cast<uint8_t*>(mapped), total_bytes,
                     /*is_creator=*/true);
}

ShmSegment::~ShmSegment() {
  if (base_ != nullptr) {
    munmap(base_, size_);
    if (is_creator_) {
      shm_unlink(name_.c_str());
    }
  }
}

ShmSegment::ShmSegment(ShmSegment&& other) noexcept
    : name_(std::move(other.name_)),
      base_(other.base_),
      size_(other.size_),
      is_creator_(other.is_creator_) {
  other.base_ = nullptr;
  other.size_ = 0;
  other.is_creator_ = false;
}

ShmSegment& ShmSegment::operator=(ShmSegment&& other) noexcept {
  if (this != &other) {
    if (base_ != nullptr) {
      munmap(base_, size_);
      if (is_creator_) {
        shm_unlink(name_.c_str());
      }
    }
    name_ = std::move(other.name_);
    base_ = other.base_;
    size_ = other.size_;
    is_creator_ = other.is_creator_;
    other.base_ = nullptr;
    other.size_ = 0;
    other.is_creator_ = false;
  }
  return *this;
}

float* ShmSegment::slot_floats(uint64_t byte_offset) const {
  assert(byte_offset % 4 == 0);
  return reinterpret_cast<float*>(base_ + byte_offset);
}

}  // namespace mmm
