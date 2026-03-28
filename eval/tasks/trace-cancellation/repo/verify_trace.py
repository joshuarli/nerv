#!/usr/bin/env python3
"""
Verify that the model's cancellation flow analysis identifies the key issues.

The cancel_order flow has three design problems:
1. Inventory released before refund — if refund fails, items are back in
   stock but customer still charged
2. Cancellation notification sent before refund completes — customer told
   "cancelled" but may not get money back
3. No rollback on partial failure — if refund fails after inventory release
   and notification, system is in inconsistent state

The answer must identify at least the ordering/atomicity issue (1 or 3)
and the premature notification issue (2).
"""

import sys
from pathlib import Path

ANSWER_FILE = "answer.md"


def main():
    if not Path(ANSWER_FILE).exists():
        print(f"FAIL: {ANSWER_FILE} not found")
        sys.exit(1)

    content = Path(ANSWER_FILE).read_text().lower()

    if len(content.strip()) < 200:
        print(f"FAIL: answer too short ({len(content.strip())} chars)")
        sys.exit(1)

    # Must trace the cancel flow through the right modules
    required_modules = ["inventory", "payment", "notification"]
    missing = [m for m in required_modules if m not in content]
    if missing:
        print(f"FAIL: answer doesn't mention all modules in cancel flow: missing {missing}")
        sys.exit(1)

    # Must identify the ordering/atomicity issue
    ordering_indicators = [
        "before refund",
        "before the refund",
        "refund fail",
        "refund fails",
        "inconsistent",
        "partial failure",
        "atomicity",
        "no rollback",
        "rollback",
        "no transaction",
        "not atomic",
        "non-atomic",
        "released before",
        "release before",
        "out of order",
        "inventory is released",
        "already released",
    ]
    found_ordering = [t for t in ordering_indicators if t in content]
    if not found_ordering:
        print("FAIL: answer doesn't identify the ordering/atomicity issue")
        print("  (inventory released before refund, no rollback on partial failure)")
        sys.exit(1)

    # Must identify the premature notification issue
    notification_indicators = [
        "notification before",
        "notif",
        "email before",
        "notify before",
        "cancelled before",
        "premature",
        "before refund is processed",
        "before the refund",
        "customer is notified",
        "sends.*cancel.*before",
        "notice.*before",
    ]
    found_notification = [t for t in notification_indicators if t in content]
    if not found_notification:
        print("FAIL: answer doesn't identify the premature notification issue")
        print("  (cancellation notice sent before refund completes)")
        sys.exit(1)

    # Should NOT claim shipping is involved in cancellation
    shipping_false_positive = (
        "shipping" in content
        and ("cancel" in content.split("shipping")[0][-50:] or "cancel" in content.split("shipping")[-1][:50])
    )
    # This is a soft check — just warn, don't fail
    if shipping_false_positive:
        print("WARN: answer may incorrectly involve shipping in the cancellation flow")

    print(f"PASS: identified ordering issue ({found_ordering[:2]}) and notification issue ({found_notification[:2]})")
    sys.exit(0)


if __name__ == "__main__":
    main()
