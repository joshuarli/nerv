#!/usr/bin/env python3
"""Print compaction statistics across all sessions in a repository.

Usage:
    scripts/compaction-stats.py                 # auto-detect from cwd
    scripts/compaction-stats.py ~/.nerv/repos/<repo_id>/sessions.db
    scripts/compaction-stats.py --all           # all repos
"""

import json
import os
import sqlite3
import sys
from pathlib import Path


def nerv_dir():
    return Path(os.environ.get("NERV_DIR", Path.home() / ".nerv"))


def find_dbs():
    """Find all sessions.db files under ~/.nerv/."""
    root = nerv_dir()
    dbs = []
    # repos/<id>/sessions.db
    repos = root / "repos"
    if repos.is_dir():
        for d in repos.iterdir():
            db = d / "sessions.db"
            if db.exists():
                dbs.append(db)
    # fallback sessions.db at nerv root
    fallback = root / "sessions.db"
    if fallback.exists():
        dbs.append(fallback)
    return dbs


def repo_fingerprint_for_cwd():
    """Get the repo fingerprint (SHA of initial commit) for the current dir."""
    import subprocess
    try:
        out = subprocess.check_output(
            ["git", "rev-list", "--max-parents=0", "HEAD"],
            stderr=subprocess.DEVNULL,
        ).decode().strip()
        # Use first line (first root commit)
        return out.splitlines()[0][:16] if out else None
    except Exception:
        return None


def db_for_cwd():
    """Find the sessions.db for the current working directory."""
    fpr = repo_fingerprint_for_cwd()
    if fpr:
        db = nerv_dir() / "repos" / fpr / "sessions.db"
        if db.exists():
            return db
    # fallback
    fb = nerv_dir() / "sessions.db"
    if fb.exists():
        return fb
    return None


def fmt_tokens(n):
    if n >= 1_000_000:
        return f"{n / 1_000_000:.1f}M"
    if n >= 1_000:
        return f"{n / 1_000:.1f}k"
    return str(n)


def analyze_db(db_path):
    """Extract compaction entries from a sessions.db file."""
    conn = sqlite3.connect(str(db_path))
    conn.row_factory = sqlite3.Row

    sessions = {}
    for row in conn.execute(
        "SELECT id, cwd, created_at, updated_at, preview, name FROM sessions ORDER BY updated_at DESC"
    ):
        sessions[row["id"]] = {
            "cwd": row["cwd"],
            "created_at": row["created_at"],
            "updated_at": row["updated_at"],
            "preview": row["preview"] or "",
            "name": row["name"] or "",
        }

    compactions = []
    for row in conn.execute("SELECT session_id, data FROM entries ORDER BY seq"):
        try:
            entry = json.loads(row["data"])
        except json.JSONDecodeError:
            continue
        if entry.get("type") != "compaction":
            continue
        entry["_session_id"] = row["session_id"]
        compactions.append(entry)

    conn.close()
    return sessions, compactions


def print_db_stats(db_path, sessions, compactions):
    """Print compaction stats for one database."""
    print(f"Database: {db_path}")
    print(f"Sessions: {len(sessions)}")
    print(f"Compaction events: {len(compactions)}")
    print()

    if not compactions:
        print("  No compaction events found.\n")
        return

    # Group by session
    by_session = {}
    for c in compactions:
        sid = c["_session_id"]
        by_session.setdefault(sid, []).append(c)

    # Aggregate stats
    full_count = 0
    lite_count = 0
    total_saved = 0
    total_lite_zeroed = 0
    total_cost_before = 0.0

    for sid, events in sorted(by_session.items(), key=lambda x: sessions.get(x[0], {}).get("updated_at", "")):
        sess = sessions.get(sid, {})
        name = sess.get("name") or sid[:14]
        cwd = sess.get("cwd", "")
        short_cwd = cwd.replace(str(Path.home()), "~") if cwd else ""

        print(f"  Session: {name}  {short_cwd}")
        for c in events:
            ctype = c.get("compaction_type", "full")
            tb = c.get("tokens_before", 0)
            ta = c.get("tokens_after", 0)
            saved = tb - ta
            ts = (c.get("timestamp") or "")[:19]
            model = c.get("model_id", "")
            cost = c.get("cost_usd_before", 0.0)
            zeroed = c.get("lite_compact_zeroed", 0)

            if ctype == "lite":
                lite_count += 1
                total_lite_zeroed += zeroed
                print(
                    f"    [{ts}] lite-compact: {fmt_tokens(tb)} -> {fmt_tokens(ta)}"
                    f"  zeroed={zeroed}"
                )
            else:
                full_count += 1
                total_saved += saved
                total_cost_before += cost
                model_label = f"  via {model}" if model else ""
                cost_label = f"  cost_before=${cost:.4f}" if cost else ""
                summary = c.get("summary", "")
                summary_preview = (summary[:80] + "...") if len(summary) > 80 else summary
                summary_preview = summary_preview.replace("\n", " ")
                print(
                    f"    [{ts}] full: {fmt_tokens(tb)} -> {fmt_tokens(ta)}"
                    f"  saved {fmt_tokens(saved)}{model_label}{cost_label}"
                )
                if summary_preview:
                    print(f"      {summary_preview}")
        print()

    # Summary
    print("Totals:")
    print(f"  Full compactions:  {full_count}")
    print(f"  Lite compactions:  {lite_count}")
    if full_count:
        print(f"  Tokens saved (full): {fmt_tokens(total_saved)}")
    if lite_count:
        print(f"  Tool results zeroed (lite): {total_lite_zeroed}")
    if total_cost_before > 0:
        print(f"  Cost before compactions: ${total_cost_before:.4f}")
    print()


def main():
    if len(sys.argv) > 1 and sys.argv[1] == "--all":
        dbs = find_dbs()
        if not dbs:
            print("No sessions.db files found under ~/.nerv/", file=sys.stderr)
            sys.exit(1)
        for db_path in dbs:
            sessions, compactions = analyze_db(db_path)
            print_db_stats(db_path, sessions, compactions)
    elif len(sys.argv) > 1 and not sys.argv[1].startswith("-"):
        db_path = Path(sys.argv[1])
        if not db_path.exists():
            print(f"error: {db_path} does not exist", file=sys.stderr)
            sys.exit(1)
        sessions, compactions = analyze_db(db_path)
        print_db_stats(db_path, sessions, compactions)
    else:
        db_path = db_for_cwd()
        if not db_path:
            print("No sessions.db found for current directory.", file=sys.stderr)
            print("Usage: scripts/compaction-stats.py [path/to/sessions.db | --all]", file=sys.stderr)
            sys.exit(1)
        sessions, compactions = analyze_db(db_path)
        print_db_stats(db_path, sessions, compactions)


if __name__ == "__main__":
    main()
