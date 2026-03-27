"""Reference implementation."""
import threading
from collections import deque


class Closed(Exception):
    pass


class BoundedBuffer:
    def __init__(self, capacity):
        self._buf = deque()
        self._capacity = capacity
        self._lock = threading.Lock()
        self._not_empty = threading.Condition(self._lock)
        self._not_full = threading.Condition(self._lock)
        self._closed = False

    def put(self, item, timeout=None):
        with self._not_full:
            if self._closed:
                raise Closed()
            if len(self._buf) >= self._capacity:
                self._not_full.wait(timeout=timeout)
                if self._closed:
                    raise Closed()
                if len(self._buf) >= self._capacity:
                    return False
            self._buf.append(item)
            self._not_empty.notify()
            return True

    def get(self, timeout=None):
        with self._not_empty:
            if not self._buf and self._closed:
                raise Closed()
            if not self._buf:
                self._not_empty.wait(timeout=timeout)
                if not self._buf and self._closed:
                    raise Closed()
                if not self._buf:
                    return None
            item = self._buf.popleft()
            self._not_full.notify()
            return item

    def close(self):
        with self._lock:
            self._closed = True
            self._not_empty.notify_all()
            self._not_full.notify_all()

    def qsize(self):
        with self._lock:
            return len(self._buf)

    @property
    def closed(self):
        return self._closed
