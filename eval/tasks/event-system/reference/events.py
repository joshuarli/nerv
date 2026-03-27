"""Reference implementation — validates that the tests are self-consistent."""


class EventBus:
    def __init__(self):
        self._handlers = {}
        self._seq = 0
        self._next_handle = 0
        self._removed = set()

    def on(self, event_name, handler, priority=0):
        handle = self._next_handle
        self._next_handle += 1
        if event_name not in self._handlers:
            self._handlers[event_name] = []
        self._handlers[event_name].append((priority, self._seq, handler, handle))
        self._seq += 1
        self._handlers[event_name].sort(key=lambda x: (-x[0], x[1]))
        return handle

    def off(self, handle):
        self._removed.add(handle)
        for event_name in self._handlers:
            self._handlers[event_name] = [
                h for h in self._handlers[event_name] if h[3] != handle
            ]

    def emit(self, event_name, data=None):
        results = []
        handlers = list(self._handlers.get(event_name, []))
        for priority, seq, handler, handle in handlers:
            if handle in self._removed:
                continue
            rv = handler(event_name, data)
            if rv is not None:
                results.append(rv)
        return results

    def once(self, event_name, handler, priority=0):
        handle = [None]

        def wrapper(name, data):
            self.off(handle[0])
            return handler(name, data)

        handle[0] = self.on(event_name, wrapper, priority)
        return handle[0]
