// Unit test for the module's PCL-free group helpers (MmmGroups.h): row
// partitioning by group name, window-id sanitisation and collision
// detection, and the Target Frames wildcard filter.

#include <cstdio>
#include <string>
#include <vector>

#include "MmmGroups.h"
#include "test/test_util.h"

int main() {
  using namespace mmm_groups;

  // Partition: first-appearance order, default group is "".
  auto g = partition({"L", "", "R", "L", "", "Ha"});
  CHECK(g.size() == 4);
  CHECK(g[0].name == "L" && g[0].rows == std::vector<size_t>({0, 3}));
  CHECK(g[1].name == "" && g[1].rows == std::vector<size_t>({1, 4}));
  CHECK(g[2].name == "R" && g[2].rows == std::vector<size_t>({2}));
  CHECK(g[3].name == "Ha" && g[3].rows == std::vector<size_t>({5}));
  auto single = partition({"", "", ""});
  CHECK(single.size() == 1 && single[0].name.empty() && single[0].rows.size() == 3);
  CHECK(partition({}).empty());

  // Names and window ids.
  CHECK(sanitize("R-1") == "R_1");
  CHECK(sanitize("S II") == "S_II");
  CHECK(sanitize("Ha") == "Ha");
  CHECK(window_id("", "MegaMergeMosaic") == "MegaMergeMosaic");
  CHECK(window_id("Ha", "MegaMergeMosaic") == "MegaMergeMosaic_Ha");
  CHECK(window_id("O-III", "seam_map") == "seam_map_O_III");
  CHECK(display_name("") == "(default)");
  CHECK(display_name("L") == "L");
  CHECK(trim("  Ha \t") == "Ha");

  // Collisions: distinct names, same id; case is preserved (Ha != ha).
  std::string a, b, id;
  CHECK(find_window_collision(partition({"R-1", "R_1"}), "MegaMergeMosaic", a, b, id));
  CHECK(a == "R-1" && b == "R_1" && id == "MegaMergeMosaic_R_1");
  CHECK(!find_window_collision(partition({"Ha", "ha", ""}), "MegaMergeMosaic", a, b, id));
  // A group literally named "MegaMergeMosaic" collides with nothing (its id
  // is MegaMergeMosaic_MegaMergeMosaic).
  CHECK(!find_window_collision(partition({"", "MegaMergeMosaic"}), "MegaMergeMosaic", a, b, id));

  // Wildcards.
  CHECK(wildcard_match("*_Ha*", "M42_Ha_p01.xisf"));
  CHECK(wildcard_match("*_ha*", "M42_Ha_p01.xisf"));
  CHECK(!wildcard_match("*_Ha*", "M42_OIII_p01.xisf"));
  CHECK(wildcard_match("p?.xisf", "p1.xisf"));
  CHECK(!wildcard_match("p?.xisf", "p12.xisf"));
  CHECK(wildcard_match("", "anything"));
  CHECK(wildcard_match("*", ""));
  CHECK(wildcard_match("Ha", "ha"));

  std::fprintf(stderr, "test_groups: OK\n");
  return 0;
}
