"""Export fetch_data results to various formats."""

import json

from core import fetch_data, fetch_data_batch


def export_json(source, timeout=30):
    """Fetch data and return as JSON string."""
    result = fetch_data(source, timeout=timeout)
    return json.dumps(result)


def export_json_batch(sources, timeout=30):
    """Fetch batch and return as JSON array string."""
    results = fetch_data_batch(sources, timeout=timeout)
    return json.dumps(results)


def export_csv(sources, timeout=30):
    """Fetch batch and return as CSV."""
    results = fetch_data_batch(sources, timeout=timeout)
    lines = ["source,timeout,data"]
    for r in results:
        lines.append(f"{r['source']},{r['timeout']},{r['data']}")
    return "\n".join(lines)
