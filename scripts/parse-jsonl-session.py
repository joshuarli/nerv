#!/usr/bin/env python3
"""Parse and summarize a nerv JSONL session export.

Usage:
    scripts/parse-jsonl-session.py <session.jsonl>
    scripts/parse-jsonl-session.py ~/.nerv/exports/d98b005b.jsonl
    nerv --export-jsonl <id> | scripts/parse-jsonl-session.py -

Exit codes:
    0  success
    1  bad arguments
    2  parse error (malformed JSONL)
"""

import json
import sys
from collections import Counter


def load(path):
    if path == "-":
        raw = sys.stdin.read()
    else:
        with open(path) as f:
            raw = f.read()
    entries = []
    for i, line in enumerate(raw.splitlines(), 1):
        line = line.strip()
        if not line:
            continue
        try:
            entries.append(json.loads(line))
        except json.JSONDecodeError as e:
            print(f"error: line {i}: {e}", file=sys.stderr)
            sys.exit(2)
    return entries


def content_text(content):
    """Extract plain text from a content array."""
    parts = []
    for c in content:
        if isinstance(c, dict):
            t = c.get("type")
            if t == "text":
                parts.append(c.get("text", ""))
            elif t == "toolResult":
                for inner in c.get("content", []):
                    if isinstance(inner, dict) and inner.get("type") == "text":
                        parts.append(inner.get("text", ""))
    return "".join(parts)


def msg_text(msg):
    return content_text(msg.get("content", []))


def trunc(s, n=120):
    s = s.replace("\n", " ").strip()
    if len(s) > n:
        return s[:n] + "…"
    return s


def fmt_tokens(n):
    if n >= 1_000_000:
        return f"{n/1_000_000:.1f}M"
    if n >= 1_000:
        return f"{n/1_000:.1f}k"
    return str(n)


def print_archived(archived_messages, indent="  "):
    """Print a compaction's archived transcript."""
    for msg in archived_messages:
        role = msg.get("role", "?")
        content = msg.get("content", [])
        if role in ("user", "User"):
            text = content_text(content)
            if text.strip():
                print(f"{indent}[user] {trunc(text, 100)}")
        elif role in ("assistant", "Assistant"):
            for block in content:
                if not isinstance(block, dict):
                    continue
                if block.get("type") == "text" and block.get("text", "").strip():
                    print(f"{indent}[asst] {trunc(block['text'], 100)}")
                elif block.get("type") == "toolCall":
                    name = block.get("name", "?")
                    args = block.get("arguments", {})
                    arg_str = args.get("path") or args.get("command") or args.get("pattern") or ""
                    print(f"{indent}[tool] {name}  {trunc(arg_str, 80)}")
        elif role in ("tool_result", "ToolResult"):
            text = content_text(content)
            err = msg.get("is_error", False)
            marker = "ERROR>" if err else "    >"
            if text.strip():
                print(f"{indent}{marker} {trunc(text, 80)}")


def main():
    if len(sys.argv) < 2:
        print(__doc__.strip(), file=sys.stderr)
        sys.exit(1)

    entries = load(sys.argv[1])

    # Separate by type
    session_meta = next((e for e in entries if e.get("type") == "session"), None)
    summary_meta = next((e for e in entries if e.get("type") == "summary"), None)
    messages = [e for e in entries if e.get("type") == "message"]
    compactions = [e for e in entries if e.get("type") == "compaction"]
    model_changes = [e for e in entries if e.get("type") == "model_change"]
    system_prompts = [e for e in entries if e.get("type") == "system_prompt"]

    # Header
    if session_meta:
        sid = session_meta.get("id", "?")[:14]
        cwd = session_meta.get("cwd", "")
        ts = session_meta.get("timestamp", "")[:19]
        ver = session_meta.get("version", "?")
        print(f"Session {sid}  {ts}  {cwd}  (v{ver})")
    print()

    # Walk entries in order, printing a unified timeline
    all_entries = [e for e in entries if e.get("type") not in ("session", "summary")]

    tool_calls = []    # (name, arg_preview) pairs
    tool_results = []  # (is_error, preview) pairs
    api_tokens = []    # (input, output, context_used)
    user_prompts = []
    bash_as_read = []
    read_as_bash = {"sed", "head", "tail", "cat", "awk", "grep", "find"}

    print("Timeline:")
    for entry in all_entries:
        t = entry.get("type", "?")

        if t == "model_change":
            model = entry.get("model_id", "?")
            provider = entry.get("provider", "?")
            ts = entry.get("timestamp", "")[:19]
            print(f"  [model_change] → {provider}/{model}  {ts}")

        elif t == "thinking_level_change":
            level = entry.get("thinking_level", "?")
            print(f"  [thinking] → {level}")

        elif t == "system_prompt":
            tok = entry.get("token_count", 0)
            print(f"  [system_prompt] {tok} tok")

        elif t == "compaction":
            tok_before = entry.get("tokens_before", 0)
            tok_after = entry.get("tokens_after", 0)
            model = entry.get("model_id", "")
            cost = entry.get("cost_usd_before", 0.0)
            summary = entry.get("summary", "")
            archived = entry.get("archived_messages", [])
            model_label = f"  via {model}" if model else ""
            cost_label = f"  cost_before=${cost:.4f}" if cost else ""
            print(
                f"  [compaction] {fmt_tokens(tok_before)} → {fmt_tokens(tok_after)} tok"
                f"{model_label}{cost_label}"
            )
            print(f"    summary: {trunc(summary, 120)}")
            if archived:
                print(f"    archived transcript ({len(archived)} messages):")
                print_archived(archived, indent="      ")

        elif t == "message":
            msg = entry.get("message", {})
            role = msg.get("role", "?")
            tok = entry.get("tokens", {}) or {}

            if role == "user":
                text = msg_text(msg)
                user_prompts.append(text)
                print(f"  [user] {trunc(text, 100)}")

            elif role == "assistant":
                if tok:
                    inp = tok.get("input", 0)
                    out = tok.get("output", 0)
                    ctx = tok.get("context_used", 0)
                    rc = tok.get("cache_read", 0)
                    api_tokens.append((inp, out, ctx))
                    rc_label = f"  Rc{fmt_tokens(rc)}" if rc else ""
                    print(f"  [assistant] ↑{fmt_tokens(inp)} ↓{fmt_tokens(out)} ctx={fmt_tokens(ctx)}{rc_label}")
                else:
                    print(f"  [assistant]")
                for block in msg.get("content", []):
                    if not isinstance(block, dict):
                        continue
                    btype = block.get("type")
                    if btype == "text" and block.get("text", "").strip():
                        print(f"    {trunc(block['text'], 100)}")
                    elif btype == "toolCall":
                        name = block.get("name", "?")
                        args = block.get("arguments", {})
                        if name == "bash":
                            cmd = args.get("command", "")
                            tool_calls.append((name, cmd))
                            first = cmd.strip().split()[0] if cmd.strip() else ""
                            if first in read_as_bash:
                                bash_as_read.append(cmd)
                            print(f"    [tool] bash  {trunc(cmd, 80)}")
                        else:
                            arg_str = args.get("path") or args.get("pattern") or json.dumps(args)[:80]
                            tool_calls.append((name, arg_str))
                            print(f"    [tool] {name}  {trunc(arg_str, 80)}")

            elif role == "tool_result":
                text = msg_text(msg)
                err = msg.get("is_error", False)
                tool_results.append((err, text))
                marker = "    ERROR>" if err else "         >"
                print(f"{marker} {trunc(text, 80)}")

        elif t == "permission_accept":
            tool = entry.get("tool", "?")
            args = entry.get("args", "")
            print(f"  [permission] accepted {tool}: {trunc(args, 60)}")

        elif t == "label":
            print(f"  [label] {entry.get('label', '')}")

        elif t == "branch_summary":
            print(f"  [branch_summary] {trunc(entry.get('summary', ''), 80)}")

    # Tool summary
    if tool_calls:
        print()
        counts = Counter(n for n, _ in tool_calls)
        total = len(tool_calls)
        print(f"Tool calls: {total}")
        for name, count in counts.most_common():
            print(f"  {name}: {count}")

    if bash_as_read:
        print()
        print(f"Bash-as-read ({len(bash_as_read)} calls that could use read/grep):")
        for cmd in bash_as_read:
            print(f"  $ {trunc(cmd, 80)}")

    errors = [preview for err, preview in tool_results if err]
    if errors:
        print()
        print(f"Tool errors ({len(errors)}):")
        for preview in errors:
            print(f"  {trunc(preview, 100)}")

    # Compaction summary
    if compactions:
        print()
        print(f"Compactions: {len(compactions)}")
        for ce in compactions:
            b = ce.get("tokens_before", 0)
            a = ce.get("tokens_after", 0)
            saved = b - a
            pct = saved * 100 // b if b else 0
            archived_count = len(ce.get("archived_messages", []))
            print(f"  {fmt_tokens(b)} → {fmt_tokens(a)}  saved {fmt_tokens(saved)} ({pct}%)  archived={archived_count} msgs")

    # Token metrics
    if api_tokens:
        total_input = sum(i for i, _, _ in api_tokens)
        total_output = sum(o for _, o, _ in api_tokens)
        peak_ctx = max(c for _, _, c in api_tokens)
        api_calls = len(api_tokens)
        avg_input = total_input // api_calls

        print()
        print("Tokens:")
        print(f"  API calls:      {api_calls}")
        print(f"  Total input:    {fmt_tokens(total_input)}")
        print(f"  Total output:   {fmt_tokens(total_output)}")
        print(f"  Peak context:   {fmt_tokens(peak_ctx)}")
        print(f"  Avg input/call: {fmt_tokens(avg_input)}")

        print()
        print("Context progression (input per API call):")
        for i, (inp, out, ctx) in enumerate(api_tokens):
            bar_len = min(ctx * 50 // 200_000, 50) if ctx > 0 else 0
            bar = "#" * bar_len
            print(f"  {i+1:3d}  {ctx:>7,}  {bar}")

    # Use summary line if present (richer than recomputing from messages)
    if summary_meta:
        cr = summary_meta.get("cache_read", 0)
        cw = summary_meta.get("cache_write", 0)
        hit = summary_meta.get("cache_hit_rate", 0.0)
        hit_calls = summary_meta.get("cache_hit_calls", 0)
        max_ctx = summary_meta.get("max_context", 0)
        ctx_win = summary_meta.get("context_window", 0)
        print()
        print("Cache:")
        print(f"  Read:     {fmt_tokens(cr)}")
        print(f"  Write:    {fmt_tokens(cw)}")
        print(f"  Hit rate: {hit:.1f}%  ({hit_calls} calls)")
        if ctx_win:
            print(f"  Peak ctx: {fmt_tokens(max_ctx)}/{fmt_tokens(ctx_win)}")

    # Repeated reads
    read_paths = [arg for name, arg in tool_calls if name == "read"]
    path_counts = Counter(read_paths)
    repeated = {p: c for p, c in path_counts.items() if c > 1}
    if repeated:
        print()
        print("Repeated reads:")
        for path, count in sorted(repeated.items(), key=lambda x: -x[1]):
            print(f"  {path}: {count}x")


if __name__ == "__main__":
    main()
