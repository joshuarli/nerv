"""Event dispatch system."""


class EventBus:
    def __init__(self):
        raise NotImplementedError

    def on(self, event_name, handler, priority=0):
        """Register a handler for an event. Higher priority runs first.
        Handlers with equal priority run in registration order.
        Returns a handle that can be passed to off()."""
        raise NotImplementedError

    def off(self, handle):
        """Unregister a handler by its handle. Safe to call during dispatch."""
        raise NotImplementedError

    def emit(self, event_name, data=None):
        """Dispatch an event to all registered handlers.
        Handlers receive (event_name, data).
        Returns list of handler return values (excluding None)."""
        raise NotImplementedError

    def once(self, event_name, handler, priority=0):
        """Like on(), but auto-unregisters after first invocation."""
        raise NotImplementedError
