"""Monitoring — wraps fetch_data calls with timing and error tracking.

The Monitor class provides instrumented versions of fetch_data and
fetch_data_batch that record call counts, error counts, and cumulative
wall-clock time. Use summary() to get a snapshot of the metrics.

Example:
    m = Monitor()
    result = m.fetch_data_monitored("api")
    print(m.summary())  # {'calls': 1, 'errors': 0, 'avg_ms': 0.01}
"""

import logging
import time

from core import fetch_data, fetch_data_batch

logger = logging.getLogger(__name__)


class Monitor:
    """Wraps fetch_data calls with monitoring metrics."""

    def __init__(self):
        self.call_count = 0
        self.error_count = 0
        self.total_time_ms = 0.0
        self._history = []

    def fetch_data_monitored(self, source, timeout=30):
        """Call fetch_data with monitoring.

        Records the call in the metrics and history. If fetch_data raises,
        increments error_count and re-raises.

        Args:
            source: Source identifier.
            timeout: Fetch timeout.

        Returns:
            Result dict from fetch_data.
        """
        start = time.monotonic()
        self.call_count += 1
        try:
            result = fetch_data(source, timeout=timeout)
            elapsed = (time.monotonic() - start) * 1000
            self.total_time_ms += elapsed
            self._history.append({"source": source, "elapsed_ms": elapsed, "error": False})
            logger.debug("fetch_data_monitored: %s took %.1fms", source, elapsed)
            return result
        except Exception as e:
            elapsed = (time.monotonic() - start) * 1000
            self.total_time_ms += elapsed
            self.error_count += 1
            self._history.append({"source": source, "elapsed_ms": elapsed, "error": True})
            logger.warning("fetch_data_monitored: %s failed after %.1fms: %s", source, elapsed, e)
            raise

    def fetch_data_batch_monitored(self, sources, timeout=30):
        """Call fetch_data_batch with monitoring.

        Records aggregate metrics for the batch. Individual source failures
        are counted as errors.

        Args:
            sources: List of source identifiers.
            timeout: Shared fetch timeout.

        Returns:
            List of result dicts.
        """
        start = time.monotonic()
        self.call_count += len(sources)
        try:
            result = fetch_data_batch(sources, timeout=timeout)
            elapsed = (time.monotonic() - start) * 1000
            self.total_time_ms += elapsed
            logger.debug("fetch_data_batch_monitored: %d sources took %.1fms", len(sources), elapsed)
            return result
        except Exception as e:
            elapsed = (time.monotonic() - start) * 1000
            self.total_time_ms += elapsed
            self.error_count += len(sources)
            logger.warning("fetch_data_batch_monitored: batch failed: %s", e)
            raise

    def summary(self):
        """Return a snapshot of monitoring metrics."""
        return {
            "calls": self.call_count,
            "errors": self.error_count,
            "avg_ms": self.total_time_ms / max(self.call_count, 1),
        }

    def history(self):
        """Return the full call history."""
        return list(self._history)

    def reset(self):
        """Clear all metrics and history."""
        self.call_count = 0
        self.error_count = 0
        self.total_time_ms = 0.0
        self._history.clear()
