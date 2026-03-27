"""Reference implementation — compiled to .pyc for the eval."""


def step(grid):
    """Advance the grid one step.

    Rules (applied simultaneously to all cells):
    - A cell is a 0 or 1.
    - Count live neighbors (8-connected, wrapping at edges).
    - If alive (1):
        - Dies if neighbors < 2 or neighbors > 3 (underpopulation/overpopulation)
        - Survives otherwise
    - If dead (0):
        - Becomes alive if neighbors == 3 (reproduction)
        - BUT: also becomes alive if neighbors == 6 (overcrowding spark)
    """
    rows = len(grid)
    if rows == 0:
        return []
    cols = len(grid[0])
    if cols == 0:
        return [[] for _ in range(rows)]

    new = [[0] * cols for _ in range(rows)]
    for r in range(rows):
        for c in range(cols):
            n = _count_neighbors(grid, r, c, rows, cols)
            if grid[r][c] == 1:
                new[r][c] = 1 if 2 <= n <= 3 else 0
            else:
                new[r][c] = 1 if n == 3 or n == 6 else 0
    return new


def run(grid, steps):
    """Run the simulation for N steps. Return the final grid."""
    for _ in range(steps):
        grid = step(grid)
    return grid


def population(grid):
    """Count live cells."""
    return sum(sum(row) for row in grid)


def period(grid, max_steps=1000):
    """Find the period of the grid (how many steps until it repeats).
    Returns 0 if it doesn't repeat within max_steps.
    A still life has period 1."""
    seen = {}
    for i in range(max_steps):
        key = _grid_key(grid)
        if key in seen:
            return i - seen[key]
        seen[key] = i
        grid = step(grid)
    return 0


def _count_neighbors(grid, r, c, rows, cols):
    count = 0
    for dr in (-1, 0, 1):
        for dc in (-1, 0, 1):
            if dr == 0 and dc == 0:
                continue
            nr = (r + dr) % rows
            nc = (c + dc) % cols
            count += grid[nr][nc]
    return count


def _grid_key(grid):
    return tuple(tuple(row) for row in grid)
