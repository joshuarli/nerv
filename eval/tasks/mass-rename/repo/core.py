"""Core utilities used throughout the project."""


def fetch_data(source, timeout=30):
    """Fetch data from the given source with a timeout."""
    if not source:
        raise ValueError("source cannot be empty")
    if timeout <= 0:
        raise ValueError("timeout must be positive")
    return {"source": source, "timeout": timeout, "data": f"content_from_{source}"}


def fetch_data_batch(sources, timeout=30):
    """Fetch data from multiple sources."""
    results = []
    for src in sources:
        results.append(fetch_data(src, timeout=timeout))
    return results


def fetch_data_cached(source, cache, timeout=30):
    """Fetch data with caching — return cached result if available."""
    if source in cache:
        return cache[source]
    result = fetch_data(source, timeout=timeout)
    cache[source] = result
    return result
