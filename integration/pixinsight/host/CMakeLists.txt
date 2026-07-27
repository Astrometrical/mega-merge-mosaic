cmake_minimum_required(VERSION 3.16)
project(mmm_host CXX)
set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
add_compile_options(-Wall -Wextra -fPIC)
find_package(Threads REQUIRED)

add_library(mmm_host STATIC mmm_shm.cpp mmm_protocol.cpp mmm_host.cpp)
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

# Golden aligned byte-identity test (Task 7): drives the real worker two ways
# (Files-mode reference vs Aligned-mode shm serve) and asserts byte-identity.
# `gen_fixtures` writes the deterministic 2-panel aligned scene AND (Task 8)
# the two-panel raw solved scene both golden tests below consume.
add_custom_target(gen_fixtures
  COMMAND cargo run --quiet --manifest-path ${MMM_CARGO_MANIFEST} -p mmm-ipc-worker --example gen_fixtures -- ${CMAKE_BINARY_DIR}/fixtures
  BYPRODUCTS ${CMAKE_BINARY_DIR}/fixtures/meta.json ${CMAKE_BINARY_DIR}/fixtures/solved_meta.json
  COMMENT "Generating aligned + solved golden fixtures")

add_executable(test_golden_aligned test/test_golden_aligned.cpp)
target_link_libraries(test_golden_aligned PRIVATE mmm_host)
add_dependencies(test_golden_aligned gen_fixtures)
add_test(NAME test_golden_aligned COMMAND test_golden_aligned ${CMAKE_BINARY_DIR}/fixtures ${MMM_WORKER})

# Golden solved-mode byte-identity test (Task 8): drives the real worker two
# ways (Files(Solved)-mode reference vs Solved-mode shm serve, with
# Host::probe_frame sizing the output slots) and asserts byte-identity.
# `gen_fixtures` (extended for Task 8) also writes the solved fixtures this
# test consumes.
add_executable(test_golden_solved test/test_golden_solved.cpp)
target_link_libraries(test_golden_solved PRIVATE mmm_host)
add_dependencies(test_golden_solved gen_fixtures)
add_test(NAME test_golden_solved COMMAND test_golden_solved ${CMAKE_BINARY_DIR}/fixtures ${MMM_WORKER})

# Fault-isolation tests (Task 9): worker crash (exit-without-Done via
# /bin/false) and mid-run cancel(), both against the real Host driver. No
# fixtures needed -- panels are synthesized in-memory.
add_executable(test_isolation test/test_isolation.cpp)
target_link_libraries(test_isolation PRIVATE mmm_host)
add_test(NAME test_isolation COMMAND test_isolation ${MMM_WORKER})
