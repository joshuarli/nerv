"""Thread-safe bounded buffer with blocking put/get and graceful shutdown."""
import threading


class Closed(Exception):
    """Raised when operating on a closed buffer."""
    pass


class BoundedBuffer:
    """A fixed-capacity buffer safe for multiple producers and consumers.

    - put() blocks if full (until space or timeout)
    - get() blocks if empty (until item or timeout)
    - close() signals no more items; blocked threads wake and raise Closed
    - Ordering: items come out in the order they were put in (FIFO)
    """

    def __init__(self, capacity):
        raise NotImplementedError

    def put(self, item, timeout=None):
        """Add item. Blocks if full. Raises Closed if buffer is closed.
        Returns True on success, False on timeout."""
        raise NotImplementedError

    def get(self, timeout=None):
        """Remove and return item. Blocks if empty. Raises Closed if
        buffer is closed AND empty. Returns None on timeout."""
        raise NotImplementedError

    def close(self):
        """Signal that no more items will be added. Consumers can still
        drain remaining items. Blocked puts raise Closed immediately.
        Blocked gets raise Closed only when buffer is empty."""
        raise NotImplementedError

    def qsize(self):
        """Current number of items in the buffer."""
        raise NotImplementedError

    @property
    def closed(self):
        """Whether close() has been called."""
        raise NotImplementedError
