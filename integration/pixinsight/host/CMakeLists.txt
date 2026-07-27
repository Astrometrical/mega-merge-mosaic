cmake_minimum_required(VERSION 3.16)
project(mmm_host CXX)
set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
add_compile_options(-Wall -Wextra -fPIC)
find_package(Threads REQUIRED)

# NOTE: mmm_host.cpp is created in Task 6; until then this library compiles
# with mmm_shm.cpp + mmm_protocol.cpp so this task is self-contained.
add_library(mmm_host STATIC mmm_shm.cpp mmm_protocol.cpp)
target_include_directories(mmm_host PUBLIC ${CMAKE_CURRENT_SOURCE_DIR} ${CMAKE_CURRENT_SOURCE_DIR}/third_party)
target_link_libraries(mmm_host PUBLIC Threads::Threads rt)

enable_testing()
# Path to the debug worker binary, overridable; default assumes cargo target dir.
set(MMM_WORKER "${CMAKE_CURRENT_SOURCE_DIR}/../../../target/debug/mmm-ipc-worker" CACHE FILEPATH "worker binary")
set(MMM_CARGO_MANIFEST "${CMAKE_CURRENT_SOURCE_DIR}/../../../Cargo.toml")

foreach(t test_shm test_protocol)
  add_executable(${t} test/${t}.cpp)
  target_link_libraries(${t} PRIVATE mmm_host)
  add_test(NAME ${t} COMMAND ${t})
endforeach()
