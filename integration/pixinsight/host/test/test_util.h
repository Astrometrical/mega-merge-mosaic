#pragma once
#include <cstdio>
#include <cstdlib>
#define CHECK(cond) do { if(!(cond)) { \
  std::fprintf(stderr, "CHECK failed: %s (%s:%d)\n", #cond, __FILE__, __LINE__); \
  std::exit(1); } } while(0)
