#!/usr/bin/env python3
"""
Verify that the model wrote a meaningful answer about the codebase.

Tool ordering is checked by the eval harness goals system, which has
access to the full trace. This script only checks the answer content.
"""

import sys
from pathlib import Path

ANSWER_FILE = "answer.md"


def main():
    if not Path(ANSWER_FILE).exists():
        print(f"FAIL: {ANSWER_FILE} not found — model did not write an answer")
        sys.exit(1)

    content = Path(ANSWER_FILE).read_text().lower()

    if len(content.strip()) < 100:
        print(f"FAIL: answer too short ({len(content.strip())} chars)")
        sys.exit(1)

    # Must mention the key types from the codebase
    required = ["pipeline", "filter"]
    missing = [r for r in required if r not in content]
    if missing:
        print(f"FAIL: answer missing key terms: {missing}")
        sys.exit(1)

    # Must describe the data flow (pipeline processes items through filters)
    flow_terms = ["process", "accept", "transform", "batch", "chain", "drop", "reject"]
    found = [t for t in flow_terms if t in content]
    if len(found) < 2:
        print(f"FAIL: answer doesn't describe the data flow (found only: {found})")
        sys.exit(1)

    # Must mention the scheduler integration
    if "scheduler" not in content and "schedule" not in content and "run_scheduled" not in content:
        print("FAIL: answer doesn't mention scheduling")
        sys.exit(1)

    print(f"PASS: answer covers pipeline, filters ({found}), and scheduling")
    sys.exit(0)


if __name__ == "__main__":
    main()
