"""Core utilities used throughout the project.

This module provides the primary data fetching primitives. All other modules
depend on these functions for their data retrieval needs.

The fetch_data family of functions supports:
- Single source fetching with configurable timeout
- Batch fetching across multiple sources
- Cached fetching with an external cache dict

All functions return dicts with keys: source, timeout, data.
"""

import logging
import re

logger = logging.getLogger(__name__)

# Regex for validating source identifiers — alphanumeric, hyphens, underscores, dots
SOURCE_PATTERN = re.compile(r'^[a-zA-Z0-9._-]+$')

# Default timeout for all fetch operations (seconds)
DEFAULT_TIMEOUT = 30

# Maximum number of sources in a batch operation
MAX_BATCH_SIZE = 100


def _validate_source(source):
    """Internal validation for source identifiers."""
    if not source:
        raise ValueError("source cannot be empty")
    if not isinstance(source, str):
        raise TypeError(f"source must be a string, got {type(source).__name__}")
    if not SOURCE_PATTERN.match(source):
        raise ValueError(f"invalid source identifier: {source!r}")
    return source


def _validate_timeout(timeout):
    """Internal validation for timeout values."""
    if not isinstance(timeout, (int, float)):
        raise TypeError(f"timeout must be numeric, got {type(timeout).__name__}")
    if timeout <= 0:
        raise ValueError("timeout must be positive")
    if timeout > 300:
        logger.warning("timeout %s exceeds recommended maximum of 300s", timeout)
    return timeout


def fetch_data(source, timeout=DEFAULT_TIMEOUT):
    """Fetch data from the given source with a timeout.

    Args:
        source: A valid source identifier (alphanumeric, hyphens, underscores).
        timeout: Maximum time in seconds to wait for the response.

    Returns:
        A dict with keys 'source', 'timeout', and 'data'.

    Raises:
        ValueError: If source is empty or invalid.
        TypeError: If arguments are the wrong type.
    """
    _validate_source(source)
    _validate_timeout(timeout)
    logger.debug("fetch_data: source=%s timeout=%s", source, timeout)
    return {"source": source, "timeout": timeout, "data": f"content_from_{source}"}


def fetch_data_batch(sources, timeout=DEFAULT_TIMEOUT):
    """Fetch data from multiple sources.

    Args:
        sources: Iterable of source identifiers.
        timeout: Shared timeout for all fetches.

    Returns:
        List of result dicts, one per source.

    Raises:
        ValueError: If any source is invalid or batch exceeds MAX_BATCH_SIZE.
    """
    sources = list(sources)
    if len(sources) > MAX_BATCH_SIZE:
        raise ValueError(f"batch size {len(sources)} exceeds maximum {MAX_BATCH_SIZE}")
    results = []
    for src in sources:
        results.append(fetch_data(src, timeout=timeout))
    logger.debug("fetch_data_batch: fetched %d sources", len(results))
    return results


def fetch_data_cached(source, cache, timeout=DEFAULT_TIMEOUT):
    """Fetch data with caching — return cached result if available.

    Args:
        source: A valid source identifier.
        cache: A mutable dict used as the cache store.
        timeout: Timeout for the underlying fetch if cache misses.

    Returns:
        A result dict (from cache or fresh fetch).
    """
    if source in cache:
        logger.debug("fetch_data_cached: cache hit for %s", source)
        return cache[source]
    result = fetch_data(source, timeout=timeout)
    cache[source] = result
    logger.debug("fetch_data_cached: cache miss for %s, stored", source)
    return result
