"""Grid simulation. Implement to match the oracle's behavior."""


def step(grid):
    """Advance the grid one step. Rules must be inferred from tests."""
    raise NotImplementedError


def run(grid, steps):
    """Run the simulation for N steps. Return the final grid."""
    raise NotImplementedError


def population(grid):
    """Count live cells."""
    raise NotImplementedError


def period(grid, max_steps=1000):
    """Find the period of the grid (steps until it repeats).
    Returns 0 if it doesn't repeat within max_steps.
    A still life has period 1."""
    raise NotImplementedError
