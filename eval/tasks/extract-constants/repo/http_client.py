"""Simple HTTP response handler with retry logic."""
import time


class Response:
    def __init__(self, status, body="", headers=None):
        self.status = status
        self.body = body
        self.headers = headers or {}


def is_success(response):
    return 200 <= response.status < 300


def is_redirect(response):
    return 300 <= response.status < 400


def is_client_error(response):
    return 400 <= response.status < 500


def is_server_error(response):
    return 500 <= response.status < 600


def should_retry(response):
    """Retry on server errors and rate limiting."""
    if response.status == 429:
        return True
    if response.status == 503:
        return True
    if response.status == 502:
        return True
    return False


def get_retry_delay(response, attempt):
    """Calculate retry delay with exponential backoff."""
    if response.status == 429:
        # Rate limited — check Retry-After header
        retry_after = response.headers.get("Retry-After")
        if retry_after:
            return min(int(retry_after), 60)
        return min(2 ** attempt, 60)
    # Server error — exponential backoff capped at 60s
    return min(2 ** attempt, 60)


def classify_response(response):
    """Return a human-readable classification."""
    if response.status == 200:
        return "ok"
    if response.status == 201:
        return "created"
    if response.status == 204:
        return "no_content"
    if response.status == 301:
        return "moved_permanently"
    if response.status == 302:
        return "found"
    if response.status == 304:
        return "not_modified"
    if response.status == 400:
        return "bad_request"
    if response.status == 401:
        return "unauthorized"
    if response.status == 403:
        return "forbidden"
    if response.status == 404:
        return "not_found"
    if response.status == 429:
        return "rate_limited"
    if response.status == 500:
        return "internal_error"
    if response.status == 502:
        return "bad_gateway"
    if response.status == 503:
        return "service_unavailable"
    return f"http_{response.status}"


def execute_with_retry(request_fn, max_retries=3):
    """Execute a request function with retry logic.
    Returns the final response."""
    for attempt in range(max_retries + 1):
        response = request_fn()
        if not should_retry(response) or attempt == max_retries:
            return response
        delay = get_retry_delay(response, attempt)
        time.sleep(delay)
    return response
