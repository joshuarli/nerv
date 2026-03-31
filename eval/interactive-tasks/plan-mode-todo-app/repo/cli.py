#!/usr/bin/env python3
"""Todo CLI — add, complete, remove, and list items."""

import sys
from todo import TodoList

_list = TodoList()


def cmd_add(args):
    if not args:
        print("usage: add <text>", file=sys.stderr)
        return
    item = _list.add(" ".join(args))
    print(f"Added #{item.id}: {item.text}")


def cmd_done(args):
    if not args:
        print("usage: done <id>", file=sys.stderr)
        return
    item = _list.complete(int(args[0]))
    if item:
        print(f"Done: #{item.id}")
    else:
        print(f"No item #{args[0]}", file=sys.stderr)


def cmd_remove(args):
    if not args:
        print("usage: remove <id>", file=sys.stderr)
        return
    if _list.remove(int(args[0])):
        print(f"Removed #{args[0]}")
    else:
        print(f"No item #{args[0]}", file=sys.stderr)


def cmd_list(args):
    items = _list.all()
    if not items:
        print("(empty)")
        return
    for item in items:
        status = "✓" if item.done else "○"
        print(f"  {status} #{item.id}  {item.text}")


COMMANDS = {
    "add": cmd_add,
    "done": cmd_done,
    "remove": cmd_remove,
    "list": cmd_list,
}


def main():
    if len(sys.argv) < 2 or sys.argv[1] not in COMMANDS:
        print("commands: add | done | remove | list")
        sys.exit(1)
    COMMANDS[sys.argv[1]](sys.argv[2:])


if __name__ == "__main__":
    main()
