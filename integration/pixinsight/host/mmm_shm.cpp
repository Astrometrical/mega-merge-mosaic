#include "mmm_shm.h"

#include <cassert>
#include <stdexcept>
#include <string>
#include <utility>

#ifndef _WIN32
#include <cerrno>
#include <cstring>
#include <fcntl.h>
#include <sys/mman.h>
#include <unistd.h>
#endif

namespace mmm {

namespace {

#ifndef _WIN32
[[noreturn]] void throw_errno(const std::string& what) {
  throw std::runtime_error(what + ": " + std::strerror(errno));
}
#else
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

// Normalize the POSIX-style name (/mmm-foo) to a Windows object name
// (Local\mmm-foo) -- identical rule to crates/mmm-core/src/ipc/shm.rs's
// `win_object_name` so the C++-created mapping and the Rust worker's
// OpenFileMappingW agree on the object name for the same Init `shm_name`.
std::wstring win_object_name(const std::string& name) {
  std::string base = (!name.empty() && name[0] == '/') ? name.substr(1) : name;
  std::wstring w = L"Local\\";
  w.reserve(w.size() + base.size());
  for (char c : base) w.push_back(static_cast<wchar_t>(static_cast<unsigned char>(c)));
  return w;
}
#endif

}  // namespace

ShmSegment::ShmSegment(std::string name, uint8_t* base, uint64_t size,
                        bool is_creator)
    : name_(std::move(name)), base_(base), size_(size), is_creator_(is_creator) {}

#ifndef _WIN32

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

#else  // _WIN32

ShmSegment ShmSegment::create(const std::string& name, uint64_t total_bytes) {
  std::wstring wname = win_object_name(name);
  ULARGE_INTEGER sz;
  sz.QuadPart = total_bytes;
  // INVALID_HANDLE_VALUE requests a pagefile-backed mapping (no file on disk),
  // matching the Rust worker's CreateFileMappingW(INVALID_HANDLE_VALUE, ...).
  HANDLE h = CreateFileMappingW(INVALID_HANDLE_VALUE, nullptr, PAGE_READWRITE,
                                sz.HighPart, sz.LowPart, wname.c_str());
  if (h == nullptr) {
    throw std::runtime_error("CreateFileMappingW(" + name + ") failed: " + last_error());
  }
  void* base = MapViewOfFile(h, FILE_MAP_ALL_ACCESS, 0, 0,
                             static_cast<SIZE_T>(total_bytes));
  if (base == nullptr) {
    std::string e = last_error();
    CloseHandle(h);
    throw std::runtime_error("MapViewOfFile(" + name + ") failed: " + e);
  }
  ShmSegment s;
  s.name_ = name;
  s.is_creator_ = true;
  s.hMap_ = h;
  s.base_ = static_cast<uint8_t*>(base);
  s.size_ = total_bytes;
  return s;
}

ShmSegment::~ShmSegment() {
  // No shm_unlink analog on Windows: the mapping is handle-refcounted and is
  // reclaimed by the kernel when the last handle to it closes. We only release
  // our view + handle here.
  if (base_ != nullptr) {
    UnmapViewOfFile(base_);
  }
  if (hMap_ != nullptr) {
    CloseHandle(hMap_);
  }
}

ShmSegment::ShmSegment(ShmSegment&& other) noexcept
    : name_(std::move(other.name_)),
      base_(other.base_),
      size_(other.size_),
      is_creator_(other.is_creator_),
      hMap_(other.hMap_) {
  other.base_ = nullptr;
  other.size_ = 0;
  other.is_creator_ = false;
  other.hMap_ = nullptr;
}

ShmSegment& ShmSegment::operator=(ShmSegment&& other) noexcept {
  if (this != &other) {
    if (base_ != nullptr) {
      UnmapViewOfFile(base_);
    }
    if (hMap_ != nullptr) {
      CloseHandle(hMap_);
    }
    name_ = std::move(other.name_);
    base_ = other.base_;
    size_ = other.size_;
    is_creator_ = other.is_creator_;
    hMap_ = other.hMap_;
    other.base_ = nullptr;
    other.size_ = 0;
    other.is_creator_ = false;
    other.hMap_ = nullptr;
  }
  return *this;
}

#endif  // _WIN32

float* ShmSegment::slot_floats(uint64_t byte_offset) const {
  assert(byte_offset % 4 == 0);
  return reinterpret_cast<float*>(base_ + byte_offset);
}

}  // namespace mmm
