"""Caching layer for fetch_data calls."""

from core import fetch_data, fetch_data_cached


class DataCache:
    def __init__(self):
        self._store = {}
        self._hits = 0
        self._misses = 0

    def get(self, source, timeout=30):
        """Get data, using cache if available."""
        result = fetch_data_cached(source, self._store, timeout=timeout)
        if source in self._store:
            self._hits += 1
        else:
            self._misses += 1
        return result

    def get_fresh(self, source, timeout=30):
        """Bypass cache and fetch_data directly."""
        self._store.pop(source, None)
        result = fetch_data(source, timeout=timeout)
        self._store[source] = result
        self._misses += 1
        return result

    def stats(self):
        return {"hits": self._hits, "misses": self._misses}

    def clear(self):
        self._store.clear()
        self._hits = 0
        self._misses = 0
