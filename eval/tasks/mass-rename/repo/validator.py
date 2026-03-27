"""Validation helpers for fetch_data results."""

from core import fetch_data


def validate_result(result):
    """Check that a fetch_data result has required fields."""
    required = {"source", "timeout", "data"}
    missing = required - set(result.keys())
    if missing:
        raise ValueError(f"Missing fields: {missing}")
    return True


def validate_source(source, timeout=30):
    """Validate by actually calling fetch_data and checking the result."""
    result = fetch_data(source, timeout=timeout)
    validate_result(result)
    return result


def validate_sources(sources, timeout=30):
    """Validate multiple sources."""
    results = []
    for src in sources:
        results.append(validate_source(src, timeout=timeout))
    return results
