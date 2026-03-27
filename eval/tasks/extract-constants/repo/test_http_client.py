import ast
import http_client
from http_client import (
    Response, is_success, is_redirect, is_client_error, is_server_error,
    should_retry, get_retry_delay, classify_response, execute_with_retry,
)


def test_classification():
    assert classify_response(Response(200)) == "ok"
    assert classify_response(Response(404)) == "not_found"
    assert classify_response(Response(429)) == "rate_limited"
    assert classify_response(Response(503)) == "service_unavailable"


def test_status_ranges():
    assert is_success(Response(200))
    assert is_success(Response(201))
    assert not is_success(Response(301))
    assert is_redirect(Response(302))
    assert is_client_error(Response(404))
    assert is_server_error(Response(500))


def test_retry_logic():
    assert should_retry(Response(429))
    assert should_retry(Response(503))
    assert should_retry(Response(502))
    assert not should_retry(Response(200))
    assert not should_retry(Response(404))


def test_retry_delay():
    r = Response(429, headers={"Retry-After": "5"})
    assert get_retry_delay(r, 0) == 5

    r = Response(500)
    assert get_retry_delay(r, 0) == 1
    assert get_retry_delay(r, 3) == 8


def test_retry_delay_cap():
    r = Response(500)
    assert get_retry_delay(r, 10) == 60


def test_execute_with_retry():
    calls = []
    def fake_request():
        calls.append(1)
        if len(calls) < 3:
            return Response(503)
        return Response(200)

    resp = execute_with_retry(fake_request, max_retries=5)
    assert resp.status == 200
    assert len(calls) == 3


def test_constants_exist():
    """HTTP status codes should be defined as module-level constants,
    not magic numbers scattered through the code."""
    source = open(http_client.__file__).read()
    tree = ast.parse(source)

    # Collect all module-level constant assignments (UPPER_CASE = number)
    constants = {}
    for node in ast.iter_child_nodes(tree):
        if isinstance(node, ast.Assign) and len(node.targets) == 1:
            target = node.targets[0]
            if isinstance(target, ast.Name) and target.id.isupper():
                if isinstance(node.value, ast.Constant) and isinstance(node.value.value, int):
                    constants[target.id] = node.value.value

    # Must have constants for the status codes used in the module
    required_values = {200, 201, 204, 301, 302, 304, 400, 401, 403, 404, 429, 500, 502, 503}
    defined_values = set(constants.values())
    missing = required_values - defined_values
    assert not missing, (
        f"Missing constants for HTTP status codes: {missing}. "
        f"Define them as module-level UPPER_CASE constants (e.g. HTTP_OK = 200)."
    )

    # The magic numbers should no longer appear as bare literals in function bodies
    bare_literals = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.FunctionDef):
            for child in ast.walk(node):
                if (isinstance(child, ast.Constant)
                    and isinstance(child.value, int)
                    and child.value in required_values):
                    bare_literals.add(child.value)

    assert not bare_literals, (
        f"Found bare status code literals in function bodies: {bare_literals}. "
        f"Replace them with the module-level constants."
    )


def test_max_retry_delay_constant():
    """The max retry delay (60) should be a named constant."""
    source = open(http_client.__file__).read()
    tree = ast.parse(source)

    # Find module-level constant with value 60
    has_delay_constant = False
    for node in ast.iter_child_nodes(tree):
        if isinstance(node, ast.Assign) and len(node.targets) == 1:
            target = node.targets[0]
            if isinstance(target, ast.Name) and target.id.isupper():
                if isinstance(node.value, ast.Constant) and node.value.value == 60:
                    has_delay_constant = True

    assert has_delay_constant, (
        "The max retry delay (60) should be a named constant (e.g. MAX_RETRY_DELAY = 60)."
    )


if __name__ == "__main__":
    test_classification()
    test_status_ranges()
    test_retry_logic()
    test_retry_delay()
    test_retry_delay_cap()
    test_execute_with_retry()
    test_constants_exist()
    test_max_retry_delay_constant()
    print("All tests passed")
