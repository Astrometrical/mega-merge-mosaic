#pragma once
#include <cstdio>
#include <cstdlib>
#define CHECK(cond) do { if(!(cond)) { \
  std::fprintf(stderr, "CHECK failed: %s (%s:%d)\n", #cond, __FILE__, __LINE__); \
  std::exit(1); } } while(0)

// Portable current-process id shim: tests use it only to build unique temp
// paths / shm names, so the exact value is unimportant -- only its uniqueness
// per process. POSIX uses getpid(); Windows uses GetCurrentProcessId().
#ifdef _WIN32
#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#include <windows.h>
static inline int mmm_test_getpid() { return static_cast<int>(GetCurrentProcessId()); }
#else
#include <unistd.h>
static inline int mmm_test_getpid() { return static_cast<int>(::getpid()); }
#endif
