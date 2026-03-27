from events import EventBus


# ── Basic dispatch ──

def test_basic_emit():
    bus = EventBus()
    log = []
    bus.on("click", lambda name, data: log.append(("click", data)))
    bus.emit("click", 42)
    assert log == [("click", 42)]


def test_multiple_handlers():
    bus = EventBus()
    log = []
    bus.on("x", lambda n, d: log.append("a"))
    bus.on("x", lambda n, d: log.append("b"))
    bus.on("x", lambda n, d: log.append("c"))
    bus.emit("x")
    assert log == ["a", "b", "c"]


def test_no_handlers():
    bus = EventBus()
    result = bus.emit("nothing")
    assert result == []


def test_return_values():
    bus = EventBus()
    bus.on("q", lambda n, d: 10)
    bus.on("q", lambda n, d: None)
    bus.on("q", lambda n, d: 20)
    result = bus.emit("q")
    assert result == [10, 20]


# ── Priority ordering ──

def test_priority_high_first():
    bus = EventBus()
    log = []
    bus.on("x", lambda n, d: log.append("low"), priority=1)
    bus.on("x", lambda n, d: log.append("high"), priority=10)
    bus.on("x", lambda n, d: log.append("mid"), priority=5)
    bus.emit("x")
    assert log == ["high", "mid", "low"]


def test_equal_priority_registration_order():
    bus = EventBus()
    log = []
    bus.on("x", lambda n, d: log.append("first"), priority=5)
    bus.on("x", lambda n, d: log.append("second"), priority=5)
    bus.on("x", lambda n, d: log.append("third"), priority=5)
    bus.emit("x")
    assert log == ["first", "second", "third"]


# ── Unregister ──

def test_off():
    bus = EventBus()
    log = []
    h = bus.on("x", lambda n, d: log.append("removed"))
    bus.on("x", lambda n, d: log.append("kept"))
    bus.off(h)
    bus.emit("x")
    assert log == ["kept"]


def test_off_during_dispatch():
    """A handler unregisters another handler that hasn't run yet.
    The unregistered handler must NOT run in this dispatch cycle."""
    bus = EventBus()
    log = []
    h2 = None

    def handler1(name, data):
        log.append("h1")
        bus.off(h2)

    def handler2(name, data):
        log.append("h2")

    bus.on("x", handler1, priority=10)
    h2 = bus.on("x", handler2, priority=1)
    bus.emit("x")
    assert log == ["h1"], f"h2 should not run after being removed: {log}"


def test_off_self_during_dispatch():
    """A handler unregisters itself. Should still complete current call
    but not be called on next emit."""
    bus = EventBus()
    log = []
    handle = None

    def self_removing(name, data):
        log.append("called")
        bus.off(handle)

    handle = bus.on("x", self_removing)
    bus.emit("x")
    bus.emit("x")
    assert log == ["called"], f"should only run once: {log}"


# ── once ──

def test_once():
    bus = EventBus()
    log = []
    bus.once("x", lambda n, d: log.append("once"))
    bus.emit("x")
    bus.emit("x")
    assert log == ["once"]


def test_once_with_priority():
    bus = EventBus()
    log = []
    bus.on("x", lambda n, d: log.append("always"), priority=1)
    bus.once("x", lambda n, d: log.append("once"), priority=10)
    bus.emit("x")
    bus.emit("x")
    assert log == ["once", "always", "always"]


# ── Cascading events (emit during dispatch) ──

def test_cascade_basic():
    """Emitting an event inside a handler. The cascading event should
    be fully dispatched before remaining handlers of the outer event."""
    bus = EventBus()
    log = []

    def cascade_handler(name, data):
        log.append("cascade")

    def trigger(name, data):
        log.append("before")
        bus.emit("inner")
        log.append("after")

    bus.on("outer", trigger)
    bus.on("inner", cascade_handler)
    bus.emit("outer")
    assert log == ["before", "cascade", "after"]


def test_cascade_does_not_reenter_same_event():
    """Emitting the same event recursively should work (reentrant).
    Each level completes its handlers before returning."""
    bus = EventBus()
    log = []
    depth = 0

    def reentrant(name, data):
        nonlocal depth
        depth += 1
        log.append(f"enter-{depth}")
        if depth < 3:
            bus.emit("x", data)
        log.append(f"exit-{depth}")
        depth -= 1

    bus.on("x", reentrant)
    bus.emit("x")
    assert log == ["enter-1", "enter-2", "enter-3", "exit-3", "exit-2", "exit-1"]


def test_cascade_sees_handler_changes():
    """A handler registered during dispatch of event A should be visible
    to a cascaded emit of event A."""
    bus = EventBus()
    log = []

    def adder(name, data):
        log.append("adder")
        bus.on("target", lambda n, d: log.append("dynamic"))
        bus.emit("target")

    bus.on("setup", adder)
    bus.emit("setup")
    assert "dynamic" in log


def test_once_in_cascade():
    """A once handler triggered during a cascade should still auto-remove."""
    bus = EventBus()
    log = []

    bus.once("inner", lambda n, d: log.append("once-inner"))

    def outer(name, data):
        bus.emit("inner")
        bus.emit("inner")

    bus.on("outer", outer)
    bus.emit("outer")
    assert log == ["once-inner"]


# ── Complex interactions ──

def test_off_handler_added_during_dispatch():
    """Register a handler during dispatch, then immediately unregister it.
    It should not run."""
    bus = EventBus()
    log = []

    def adder(name, data):
        h = bus.on("x", lambda n, d: log.append("dynamic"))
        bus.off(h)
        log.append("adder")

    bus.on("x", adder, priority=10)
    bus.emit("x")
    assert log == ["adder"]


def test_mixed_priority_once_off():
    """Combine priority, once, and off in one scenario."""
    bus = EventBus()
    log = []

    h_low = bus.on("x", lambda n, d: log.append("low"), priority=1)
    bus.once("x", lambda n, d: log.append("once-high"), priority=100)
    bus.on("x", lambda n, d: log.append("mid"), priority=50)

    bus.emit("x")
    assert log == ["once-high", "mid", "low"]

    log.clear()
    bus.off(h_low)
    bus.emit("x")
    assert log == ["mid"]


if __name__ == "__main__":
    test_basic_emit()
    test_multiple_handlers()
    test_no_handlers()
    test_return_values()
    test_priority_high_first()
    test_equal_priority_registration_order()
    test_off()
    test_off_during_dispatch()
    test_off_self_during_dispatch()
    test_once()
    test_once_with_priority()
    test_cascade_basic()
    test_cascade_does_not_reenter_same_event()
    test_cascade_sees_handler_changes()
    test_once_in_cascade()
    test_off_handler_added_during_dispatch()
    test_mixed_priority_once_off()
    print("All tests passed")
