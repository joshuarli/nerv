def sum_to(n):
    """Sum integers from 1 to n (inclusive)."""
    total = 0
    for i in range(1, n):  # BUG: should be range(1, n + 1)
        total += i
    return total
