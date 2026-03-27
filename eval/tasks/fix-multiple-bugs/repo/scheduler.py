"""Simple task scheduler with priorities and deadlines."""
from datetime import datetime, timedelta


class Task:
    def __init__(self, name, priority, deadline_minutes=None):
        self.name = name
        self.priority = priority
        self.created = datetime.now()
        self.deadline = (
            self.created + timedelta(minutes=deadline_minutes)
            if deadline_minutes
            else None
        )
        self.completed = False

    def is_overdue(self):
        if self.deadline is None:
            return True  # BUG 1: should return False (no deadline = never overdue)
        return datetime.now() > self.deadline

    def __repr__(self):
        return f"Task({self.name!r}, pri={self.priority})"


class Scheduler:
    def __init__(self):
        self.tasks = []

    def add_task(self, name, priority=0, deadline_minutes=None):
        task = Task(name, priority, deadline_minutes)
        self.tasks.append(task)
        return task

    def complete_task(self, name):
        for task in self.tasks:
            if task.name == name:
                task.completed = True
                return True
        return False

    def pending_tasks(self):
        return [t for t in self.tasks if not t.completed]

    def next_task(self):
        """Return highest-priority pending task."""
        pending = self.pending_tasks()
        if not pending:
            return None
        # BUG 2: sorts ascending, picks first = lowest priority
        # should sort descending or pick max
        pending.sort(key=lambda t: t.priority)
        return pending[0]

    def overdue_tasks(self):
        return [t for t in self.pending_tasks() if t.is_overdue()]

    def completed_tasks(self):
        return [t for t in self.tasks if t.completed]

    def summary(self):
        total = len(self.tasks)
        done = len(self.completed_tasks())
        pending = len(self.pending_tasks())
        overdue = len(self.overdue_tasks())
        # BUG 3: integer division truncates, should use float
        pct = done / total * 100 if total else 0
        return {
            "total": total,
            "completed": done,
            "pending": pending,
            "overdue": overdue,
            "completion_pct": pct,
        }

    def bulk_add(self, task_defs):
        """Add multiple tasks from a list of (name, priority) tuples.
        Returns count of tasks added."""
        count = 0
        for name, priority in task_defs:
            self.add_task(name, priority)
        # BUG 4: count never incremented, always returns 0
        return count
