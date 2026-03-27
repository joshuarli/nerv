#!/usr/bin/env python3
"""Parse and summarize a nerv JSONL session export.

Usage:
    scripts/parse-jsonl-session.py <session.jsonl>
    scripts/parse-jsonl-session.py ~/.nerv/exports/d98b005b.jsonl
    nerv --export-jsonl <id> | scripts/parse-jsonl-session.py -
"""

import json
import sys
from collections import Counter


def load(path):
    if path == "-":
        return [json.loads(l) for l in sys.stdin if l.strip()]
    with open(path) as f:
        return [json.loads(l) for l in f if l.strip()]


def get_msg(entry):
    return entry.get("message", {})


def content_text(msg):
    return "".join(
        c.get("text", "") for c in msg.get("content", []) if isinstance(c, dict)
    )


def main():
    if len(sys.argv) < 2:
        print(__doc__.strip(), file=sys.stderr)
        sys.exit(1)

    entries = load(sys.argv[1])

    # Metadata
    session = next((e for e in entries if e.get("type") == "session"), None)
    if session:
        sid = session.get("id", "?")[:12]
        cwd = session.get("cwd", "")
        ts = session.get("timestamp", "")[:19]
        print(f"Session {sid}  {ts}  {cwd}")

    # Collect per-message data
    tool_calls = []  # (name, path_or_cmd)
    tool_results = []  # (is_error, preview)
    api_tokens = []  # (input, output, context_used)
    user_prompts = []
    assistant_texts = []
    bash_as_read = []  # bash calls that should have been read/grep

    read_as_bash = {"sed", "head", "tail", "cat", "awk"}

    for entry in entries:
        if entry.get("type") != "message":
            continue
        msg = get_msg(entry)
        role = msg.get("role")

        if role == "user":
            user_prompts.append(content_text(msg)[:200])

        elif role == "assistant":
            tok = entry.get("tokens", {})
            if tok:
                api_tokens.append(
                    (tok.get("input", 0), tok.get("output", 0), tok.get("context_used", 0))
                )
            for b in msg.get("content", []):
                if b.get("type") == "toolCall":
                    args = b.get("arguments", {})
                    name = b["name"]
                    if name == "bash":
                        cmd = args.get("command", "")
                        tool_calls.append((name, cmd[:80]))
                        first_word = cmd.strip().split()[0] if cmd.strip() else ""
                        if first_word in read_as_bash:
                            bash_as_read.append(cmd[:80])
                    else:
                        path = args.get("path", args.get("pattern", ""))
                        tool_calls.append((name, str(path)[:80]))
                text = b.get("text", "")
                if b.get("type") == "text" and text:
                    assistant_texts.append(text[:120])

        elif role == "tool_result":
            text = content_text(msg)
            err = msg.get("is_error", False)
            tool_results.append((err, text[:100]))

    # --- Print report ---

    if user_prompts:
        print()
        print("User prompts:")
        for p in user_prompts:
            print(f"  {p}")

    # Tool call trace
    print()
    print("Tool calls:")
    result_idx = 0
    for name, arg in tool_calls:
        print(f"  {name:8s}  {arg}")
        # Show result inline
        if result_idx < len(tool_results):
            err, preview = tool_results[result_idx]
            marker = " ERROR " if err else "     > "
            preview = preview.replace("\n", " ")[:70]
            print(f"  {marker}{preview}")
            result_idx += 1

    # Tool usage summary
    print()
    counts = Counter(n for n, _ in tool_calls)
    print(f"Tool calls: {len(tool_calls)}")
    for name, count in counts.most_common():
        print(f"  {name}: {count}")

    # Bash misuse
    if bash_as_read:
        print()
        print(f"Bash-as-read ({len(bash_as_read)} calls that should use read/grep):")
        for cmd in bash_as_read:
            print(f"  $ {cmd}")

    # Token metrics
    if api_tokens:
        total_input = sum(i for i, _, _ in api_tokens)
        total_output = sum(o for _, o, _ in api_tokens)
        peak_ctx = max(c for _, _, c in api_tokens)
        api_calls = len(api_tokens)
        avg_input = total_input // api_calls

        print()
        print("Tokens:")
        print(f"  API calls:     {api_calls}")
        print(f"  Total input:   {total_input:,}")
        print(f"  Total output:  {total_output:,}")
        print(f"  Peak context:  {peak_ctx:,}")
        print(f"  Avg input/call:{avg_input:,}")

        # Context progression
        print()
        print("Context progression (input per API call):")
        for i, (inp, out, ctx) in enumerate(api_tokens):
            bar_len = min(ctx * 50 // 200_000, 50) if ctx > 0 else 0
            bar = "#" * bar_len
            print(f"  {i+1:3d}  {ctx:>7,}  {bar}")

    # Read patterns — detect redundant reads
    read_paths = [arg for name, arg in tool_calls if name == "read"]
    path_counts = Counter(read_paths)
    repeated = {p: c for p, c in path_counts.items() if c > 1}
    if repeated:
        print()
        print("Repeated reads:")
        for path, count in sorted(repeated.items(), key=lambda x: -x[1]):
            print(f"  {path}: {count}x")

    # Errors
    errors = [(preview) for err, preview in tool_results if err]
    if errors:
        print()
        print(f"Errors ({len(errors)}):")
        for preview in errors:
            print(f"  {preview.replace(chr(10), ' ')[:80]}")


if __name__ == "__main__":
    main()
