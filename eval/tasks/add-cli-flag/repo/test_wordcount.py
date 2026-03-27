import subprocess
import tempfile
import os

def run_wc(*args):
    result = subprocess.run(
        ["python3", "wordcount.py", *args],
        capture_output=True, text=True
    )
    return result.stdout, result.stderr, result.returncode


def test_basic_count():
    with tempfile.NamedTemporaryFile(mode="w", suffix=".txt", delete=False) as f:
        f.write("hello world\nfoo bar baz\n")
        path = f.name
    try:
        out, _, rc = run_wc(path)
        assert rc == 0
        assert "2" in out  # 2 lines
        assert "5" in out  # 5 words
    finally:
        os.unlink(path)


def test_lines_only_flag():
    """The -l flag should print only line counts."""
    with tempfile.NamedTemporaryFile(mode="w", suffix=".txt", delete=False) as f:
        f.write("one\ntwo\nthree\n")
        path = f.name
    try:
        out, _, rc = run_wc("-l", path)
        assert rc == 0
        # Should show line count (3) but NOT word count
        lines = out.strip().split("\n")
        assert len(lines) == 1
        # Line should have exactly one number and the filename
        parts = lines[0].split()
        assert len(parts) == 2, f"expected '  N  file', got: {lines[0]!r}"
        assert parts[0] == "3"
    finally:
        os.unlink(path)


def test_words_only_flag():
    """The -w flag should print only word counts."""
    with tempfile.NamedTemporaryFile(mode="w", suffix=".txt", delete=False) as f:
        f.write("hello world\n")
        path = f.name
    try:
        out, _, rc = run_wc("-w", path)
        assert rc == 0
        lines = out.strip().split("\n")
        assert len(lines) == 1
        parts = lines[0].split()
        assert len(parts) == 2, f"expected '  N  file', got: {lines[0]!r}"
        assert parts[0] == "2"
    finally:
        os.unlink(path)


if __name__ == "__main__":
    test_basic_count()
    test_lines_only_flag()
    test_words_only_flag()
    print("All tests passed")
