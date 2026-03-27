import threading
import time
from buffer import BoundedBuffer, Closed


# ── Basic single-threaded ──

def test_put_get():
    b = BoundedBuffer(3)
    b.put("a")
    b.put("b")
    assert b.qsize() == 2
    assert b.get() == "a"
    assert b.get() == "b"
    assert b.qsize() == 0


def test_fifo_order():
    b = BoundedBuffer(10)
    for i in range(10):
        b.put(i)
    for i in range(10):
        assert b.get() == i


def test_get_timeout_empty():
    b = BoundedBuffer(5)
    result = b.get(timeout=0.05)
    assert result is None


def test_put_timeout_full():
    b = BoundedBuffer(1)
    b.put("x")
    result = b.put("y", timeout=0.05)
    assert result is False


def test_put_returns_true():
    b = BoundedBuffer(5)
    assert b.put("x") is True


# ── Close semantics ──

def test_close_empty_get_raises():
    b = BoundedBuffer(5)
    b.close()
    assert b.closed
    try:
        b.get()
        assert False, "should raise Closed"
    except Closed:
        pass


def test_close_put_raises():
    b = BoundedBuffer(5)
    b.close()
    try:
        b.put("x")
        assert False, "should raise Closed"
    except Closed:
        pass


def test_close_drain_remaining():
    b = BoundedBuffer(5)
    b.put("a")
    b.put("b")
    b.close()
    assert b.get() == "a"
    assert b.get() == "b"
    try:
        b.get()
        assert False, "should raise Closed after drain"
    except Closed:
        pass


# ── Concurrent ──

def test_producer_consumer():
    b = BoundedBuffer(2)
    results = []
    n = 20

    def producer():
        for i in range(n):
            b.put(i)
        b.close()

    def consumer():
        while True:
            try:
                item = b.get(timeout=1.0)
                if item is not None:
                    results.append(item)
            except Closed:
                break

    t1 = threading.Thread(target=producer)
    t2 = threading.Thread(target=consumer)
    t1.start()
    t2.start()
    t1.join(timeout=5)
    t2.join(timeout=5)

    assert sorted(results) == list(range(n)), f"lost items: {results}"


def test_multiple_producers():
    b = BoundedBuffer(3)
    results = []
    n_per_producer = 15
    n_producers = 3

    def producer(offset):
        for i in range(n_per_producer):
            b.put(offset + i)

    def consumer():
        while True:
            try:
                item = b.get(timeout=1.0)
                if item is not None:
                    results.append(item)
            except Closed:
                break

    producers = [threading.Thread(target=producer, args=(i * 100,)) for i in range(n_producers)]
    cons = threading.Thread(target=consumer)

    for p in producers:
        p.start()
    cons.start()

    for p in producers:
        p.join(timeout=5)
    b.close()
    cons.join(timeout=5)

    assert len(results) == n_per_producer * n_producers, (
        f"expected {n_per_producer * n_producers}, got {len(results)}"
    )


def test_multiple_consumers():
    b = BoundedBuffer(5)
    results = []
    lock = threading.Lock()
    n = 30

    def producer():
        for i in range(n):
            b.put(i)
        b.close()

    def consumer():
        while True:
            try:
                item = b.get(timeout=1.0)
                if item is not None:
                    with lock:
                        results.append(item)
            except Closed:
                break

    prod = threading.Thread(target=producer)
    consumers = [threading.Thread(target=consumer) for _ in range(3)]

    prod.start()
    for c in consumers:
        c.start()
    prod.join(timeout=5)
    for c in consumers:
        c.join(timeout=5)

    assert sorted(results) == list(range(n)), f"lost items: got {len(results)}"


def test_close_unblocks_waiting_get():
    b = BoundedBuffer(5)
    unblocked = threading.Event()

    def waiter():
        try:
            b.get(timeout=5.0)
        except Closed:
            unblocked.set()

    t = threading.Thread(target=waiter)
    t.start()
    time.sleep(0.1)
    b.close()
    assert unblocked.wait(timeout=2.0), "close() should unblock waiting get()"
    t.join(timeout=2)


def test_close_unblocks_waiting_put():
    b = BoundedBuffer(1)
    b.put("full")
    unblocked = threading.Event()

    def waiter():
        try:
            b.put("blocked", timeout=5.0)
        except Closed:
            unblocked.set()

    t = threading.Thread(target=waiter)
    t.start()
    time.sleep(0.1)
    b.close()
    assert unblocked.wait(timeout=2.0), "close() should unblock waiting put()"
    t.join(timeout=2)


def test_high_throughput():
    """Stress test: many items through a small buffer with multiple threads."""
    b = BoundedBuffer(4)
    results = []
    lock = threading.Lock()
    n_per_producer = 100
    n_producers = 4
    n_consumers = 4

    def producer(offset):
        for i in range(n_per_producer):
            b.put(offset + i)

    def consumer():
        while True:
            try:
                item = b.get(timeout=2.0)
                if item is not None:
                    with lock:
                        results.append(item)
            except Closed:
                break

    producers = [threading.Thread(target=producer, args=(i * 1000,)) for i in range(n_producers)]
    consumers = [threading.Thread(target=consumer) for _ in range(n_consumers)]

    for t in producers + consumers:
        t.start()
    for p in producers:
        p.join(timeout=10)
    b.close()
    for c in consumers:
        c.join(timeout=10)

    expected = n_per_producer * n_producers
    assert len(results) == expected, f"data loss: expected {expected}, got {len(results)}"
    assert len(set(results)) == expected, f"duplicates: {expected - len(set(results))}"


if __name__ == "__main__":
    test_put_get()
    test_fifo_order()
    test_get_timeout_empty()
    test_put_timeout_full()
    test_put_returns_true()
    test_close_empty_get_raises()
    test_close_put_raises()
    test_close_drain_remaining()
    test_producer_consumer()
    test_multiple_producers()
    test_multiple_consumers()
    test_close_unblocks_waiting_get()
    test_close_unblocks_waiting_put()
    test_high_throughput()
    print("All tests passed")
