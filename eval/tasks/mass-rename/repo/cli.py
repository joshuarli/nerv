"""CLI entry point — uses fetch_data to retrieve and display data."""

from core import fetch_data, fetch_data_batch


def run_cli(args):
    """Process CLI arguments and fetch data."""
    if not args:
        print("Usage: cli.py <source> [source...]")
        return 1

    if len(args) == 1:
        result = fetch_data(args[0])
        print(f"Source: {result['source']}")
        print(f"Data: {result['data']}")
    else:
        results = fetch_data_batch(args)
        for r in results:
            print(f"  {r['source']}: {r['data']}")

    return 0


if __name__ == "__main__":
    import sys
    sys.exit(run_cli(sys.argv[1:]))
