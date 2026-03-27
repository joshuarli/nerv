"""Data pipeline — transforms fetched data through a series of steps.

A Pipeline fetches data from configured sources and applies a chain of
transform functions. Transforms are applied in order; each receives the
output of the previous step.

Example:
    p = Pipeline(["api", "db"])
    p.add_transform(lambda item: {**item, "processed": True})
    results = p.run()
"""

import logging

from core import fetch_data, fetch_data_batch

logger = logging.getLogger(__name__)


class PipelineError(Exception):
    """Raised when a pipeline step fails."""
    pass


class Pipeline:
    """Configurable data pipeline with ordered transforms."""

    def __init__(self, sources):
        """Initialize with a list of source identifiers.

        Args:
            sources: List of source identifiers to fetch from.
        """
        self.sources = list(sources)
        self.transforms = []
        self._run_count = 0

    def add_transform(self, fn):
        """Add a transform function to the pipeline.

        Transforms are applied in the order they are added. Each transform
        receives a single item dict and must return a dict.

        Args:
            fn: A callable that takes and returns a dict.

        Returns:
            self, for chaining.
        """
        if not callable(fn):
            raise TypeError("transform must be callable")
        self.transforms.append(fn)
        return self

    def run(self, timeout=30):
        """Fetch data from all sources and apply transforms.

        Args:
            timeout: Timeout for the underlying fetch operations.

        Returns:
            List of transformed result dicts.

        Raises:
            PipelineError: If a transform step raises an exception.
        """
        self._run_count += 1
        logger.info("Pipeline.run #%d: fetching %d sources", self._run_count, len(self.sources))
        raw = fetch_data_batch(self.sources, timeout=timeout)
        result = raw
        for i, fn in enumerate(self.transforms):
            try:
                result = [fn(item) for item in result]
            except Exception as e:
                raise PipelineError(f"Transform step {i} failed: {e}") from e
        logger.info("Pipeline.run #%d: completed with %d results", self._run_count, len(result))
        return result

    def run_single(self, source, timeout=30):
        """Fetch data from a single source and apply transforms.

        This is a convenience method equivalent to running the pipeline with
        a single source, but avoids the batch overhead.

        Args:
            source: A single source identifier.
            timeout: Timeout for the fetch.

        Returns:
            A single transformed result dict.
        """
        item = fetch_data(source, timeout=timeout)
        for fn in self.transforms:
            item = fn(item)
        return item

    @property
    def run_count(self):
        """Number of times run() has been called."""
        return self._run_count

    def reset(self):
        """Clear transforms and reset run counter."""
        self.transforms.clear()
        self._run_count = 0
