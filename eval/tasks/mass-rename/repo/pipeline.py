"""Data pipeline — transforms fetched data through a series of steps."""

from core import fetch_data, fetch_data_batch


class Pipeline:
    def __init__(self, sources):
        self.sources = sources
        self.transforms = []

    def add_transform(self, fn):
        self.transforms.append(fn)
        return self

    def run(self, timeout=30):
        """Fetch data from all sources and apply transforms."""
        raw = fetch_data_batch(self.sources, timeout=timeout)
        result = raw
        for fn in self.transforms:
            result = [fn(item) for item in result]
        return result

    def run_single(self, source, timeout=30):
        """Fetch data from a single source and apply transforms."""
        item = fetch_data(source, timeout=timeout)
        for fn in self.transforms:
            item = fn(item)
        return item
