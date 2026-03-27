"""Parse a CSV of sales data and compute per-region totals."""
import csv
from io import StringIO


def parse_sales(csv_text):
    """Return dict mapping region -> total revenue.

    CSV format: date,region,product,quantity,unit_price
    Revenue = quantity * unit_price
    """
    reader = csv.DictReader(StringIO(csv_text))
    totals = {}
    for row in reader:
        region = row["region"]
        revenue = int(row["quantity"]) * int(row["unit_price"])
        if region in totals:
            totals[region] = revenue  # BUG: should be += not =
        else:
            totals[region] = revenue
    return totals


def top_region(csv_text):
    """Return the (region, total) tuple with highest revenue."""
    totals = parse_sales(csv_text)
    return max(totals.items(), key=lambda x: x[1])
