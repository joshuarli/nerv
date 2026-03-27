"""Retry logic for fetch_data calls."""

import time

from core import fetch_data


def fetch_data_with_retry(source, max_retries=3, timeout=30, backoff=0.1):
    """Call fetch_data with retries on failure."""
    last_error = None
    for attempt in range(max_retries + 1):
        try:
            return fetch_data(source, timeout=timeout)
        except Exception as e:
            last_error = e
            if attempt < max_retries:
                time.sleep(backoff * (2 ** attempt))
    raise last_error


def fetch_data_or_default(source, default=None, timeout=30):
    """Call fetch_data, return default on failure."""
    try:
        return fetch_data(source, timeout=timeout)
    except Exception:
        return default
