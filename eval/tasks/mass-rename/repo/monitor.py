"""Monitoring — wraps fetch_data calls with timing and error tracking."""

import time

from core import fetch_data, fetch_data_batch


class Monitor:
    def __init__(self):
        self.call_count = 0
        self.error_count = 0
        self.total_time_ms = 0

    def fetch_data_monitored(self, source, timeout=30):
        """Call fetch_data with monitoring."""
        start = time.monotonic()
        self.call_count += 1
        try:
            result = fetch_data(source, timeout=timeout)
            elapsed = (time.monotonic() - start) * 1000
            self.total_time_ms += elapsed
            return result
        except Exception:
            self.error_count += 1
            raise

    def fetch_data_batch_monitored(self, sources, timeout=30):
        """Call fetch_data_batch with monitoring."""
        start = time.monotonic()
        self.call_count += len(sources)
        try:
            result = fetch_data_batch(sources, timeout=timeout)
            elapsed = (time.monotonic() - start) * 1000
            self.total_time_ms += elapsed
            return result
        except Exception:
            self.error_count += len(sources)
            raise

    def summary(self):
        return {
            "calls": self.call_count,
            "errors": self.error_count,
            "avg_ms": self.total_time_ms / max(self.call_count, 1),
        }
