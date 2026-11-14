from pathlib import Path

from narrate.batching import MAX_ROWS, Job, batch_seed, budget, plan_batches
from narrate.profile import Reference


def ref(seconds, sha="a"):
    return Reference(Path("a.wav"), "t", seconds, sha * 64)


def jobs(n, chars, reference, take=1):
    return [Job(f"k{i}", take, "x" * chars, reference) for i in range(n)]


def sizes(batches):
    return [len(b) for b in batches]


def test_budget_values():
    assert budget(10.0) == 2393
    assert budget(49.0) == 897
    assert budget(53.0) == 843


def test_batches_follow_the_rounds():
    assert sizes(plan_batches(jobs(3, 787, ref(10.0)))) == [3]
    assert sizes(plan_batches(jobs(3, 787, ref(49.0)))) == [1, 1, 1]
    assert sizes(plan_batches(jobs(6, 144, ref(49.0)))) == [6]
    assert sizes(plan_batches(jobs(6, 557, ref(49.0)))) == [1] * 6
    assert MAX_ROWS == 6
    assert sizes(plan_batches(jobs(7, 10, ref(10.0)))) == [6, 1]
    mixed = jobs(2, 10, ref(10.0, "a")) + jobs(2, 10, ref(10.0, "b"))
    out = plan_batches(mixed)
    assert sizes(out) == [2, 2]
    assert all(len({j.reference.sha256 for j in b}) == 1 for b in out)


def test_groups_keep_first_seen_order_and_sort_by_length():
    ra, rb = ref(10.0, "a"), ref(10.0, "b")
    js = [Job("1", 1, "xxx", rb), Job("2", 1, "xxxxx", ra), Job("3", 1, "x", rb)]
    out = plan_batches(js)
    assert [[j.key for j in b] for b in out] == [["3", "1"], ["2"]]


def test_batch_seed_differs_per_take():
    r = ref(10.0)
    a, b = Job("k", 1, "x", r), Job("k", 2, "x", r)
    assert batch_seed(a) != batch_seed(b)
    assert batch_seed(a) == batch_seed(Job("k", 1, "y", r))
