#!/usr/bin/env python3
"""Count words, lines, and characters in text files."""
import argparse
import sys


def count_file(path):
    with open(path) as f:
        text = f.read()
    lines = text.count("\n")
    words = len(text.split())
    chars = len(text)
    return lines, words, chars


def main():
    parser = argparse.ArgumentParser(description="Count words, lines, chars")
    parser.add_argument("files", nargs="+", help="Files to count")
    args = parser.parse_args()

    total_lines = total_words = total_chars = 0
    for path in args.files:
        lines, words, chars = count_file(path)
        print(f"  {lines:>6} {words:>6} {chars:>6} {path}")
        total_lines += lines
        total_words += words
        total_chars += chars

    if len(args.files) > 1:
        print(f"  {total_lines:>6} {total_words:>6} {total_chars:>6} total")


if __name__ == "__main__":
    main()
