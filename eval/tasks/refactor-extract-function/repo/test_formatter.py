import ast
import inspect
from formatter import format_for_display, format_for_csv, format_for_json

USERS = [
    {"name": "  alice smith ", "email": " Alice@Example.COM "},
    {"name": "bob jones", "email": "bob"},
    {"name": " carol ", "email": " CAROL@test.org"},
]


def test_display():
    out = format_for_display(USERS)
    assert "Alice Smith <alice@example.com>" in out
    assert "Bob Jones <invalid>" in out
    assert "Carol <carol@test.org>" in out


def test_csv():
    out = format_for_csv(USERS)
    lines = out.strip().split("\n")
    assert lines[0] == "name,email"
    assert "Alice Smith,alice@example.com" in lines[1]
    assert "Bob Jones,invalid" in lines[2]


def test_json():
    out = format_for_json(USERS)
    assert out[0] == {"name": "Alice Smith", "email": "alice@example.com"}
    assert out[1] == {"name": "Bob Jones", "email": "invalid"}
    assert out[2] == {"name": "Carol", "email": "carol@test.org"}


def test_normalize_extracted():
    """The normalization logic should be extracted into a helper function
    called from all three format functions — no duplication."""
    source = inspect.getsource(inspect.getmodule(format_for_display))
    tree = ast.parse(source)

    # Count how many functions contain '.strip().title()' in source
    functions_with_title = 0
    for node in ast.walk(tree):
        if isinstance(node, ast.FunctionDef):
            func_source = ast.get_source_segment(source, node) or ""
            if ".strip().title()" in func_source:
                functions_with_title += 1

    # After refactoring, only 1 function should have the normalization
    assert functions_with_title <= 1, (
        f"Found .strip().title() in {functions_with_title} functions. "
        "Extract a normalize_user() helper and call it from all three."
    )


if __name__ == "__main__":
    test_display()
    test_csv()
    test_json()
    test_normalize_extracted()
    print("All tests passed")
