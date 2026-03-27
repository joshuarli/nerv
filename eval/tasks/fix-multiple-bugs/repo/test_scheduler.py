from scheduler import Scheduler


def test_next_task_returns_highest_priority():
    s = Scheduler()
    s.add_task("low", priority=1)
    s.add_task("high", priority=10)
    s.add_task("medium", priority=5)
    nxt = s.next_task()
    assert nxt.name == "high", f"expected 'high', got '{nxt.name}'"


def test_no_deadline_not_overdue():
    s = Scheduler()
    t = s.add_task("relaxed", priority=1)
    assert not t.is_overdue(), "task with no deadline should not be overdue"


def test_overdue_tasks_excludes_no_deadline():
    s = Scheduler()
    s.add_task("no-deadline", priority=1)
    s.add_task("far-future", priority=2, deadline_minutes=9999)
    overdue = s.overdue_tasks()
    assert len(overdue) == 0, f"expected 0 overdue, got {len(overdue)}: {overdue}"


def test_summary_completion_percentage():
    s = Scheduler()
    s.add_task("a", priority=1)
    s.add_task("b", priority=2)
    s.add_task("c", priority=3)
    s.complete_task("a")
    summary = s.summary()
    # 1 out of 3 = 33.33...%
    assert abs(summary["completion_pct"] - 33.33) < 1, (
        f"expected ~33.33%, got {summary['completion_pct']}"
    )


def test_bulk_add_returns_count():
    s = Scheduler()
    count = s.bulk_add([("x", 1), ("y", 2), ("z", 3)])
    assert count == 3, f"expected 3, got {count}"
    assert len(s.tasks) == 3


def test_complete_and_pending():
    s = Scheduler()
    s.add_task("a", 1)
    s.add_task("b", 2)
    s.complete_task("a")
    assert len(s.pending_tasks()) == 1
    assert len(s.completed_tasks()) == 1


def test_complete_nonexistent():
    s = Scheduler()
    assert s.complete_task("nope") is False


if __name__ == "__main__":
    test_next_task_returns_highest_priority()
    test_no_deadline_not_overdue()
    test_overdue_tasks_excludes_no_deadline()
    test_summary_completion_percentage()
    test_bulk_add_returns_count()
    test_complete_and_pending()
    test_complete_nonexistent()
    print("All tests passed")
