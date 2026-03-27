"""Tests compare grid.py against the compiled oracle.
The oracle is a .pyc — you cannot read its source."""
import importlib.util
import os

# Load oracle from .pyc
_oracle_path = os.path.join(os.path.dirname(__file__), "oracle.pyc")
_spec = importlib.util.spec_from_file_location("oracle", _oracle_path)
_oracle = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_oracle)

from grid import step, run, population, period


# ── step ──

def test_step_empty():
    assert step([]) == _oracle.step([])


def test_step_all_dead():
    g = [[0, 0, 0], [0, 0, 0], [0, 0, 0]]
    assert step(g) == _oracle.step(g)


def test_step_single_cell():
    g = [[0, 0, 0], [0, 1, 0], [0, 0, 0]]
    assert step(g) == _oracle.step(g)


def test_step_vertical_line_3x3():
    g = [[0, 1, 0], [0, 1, 0], [0, 1, 0]]
    assert step(g) == _oracle.step(g)


def test_step_horizontal_line_3x3():
    g = [[0, 0, 0], [1, 1, 1], [0, 0, 0]]
    assert step(g) == _oracle.step(g)


def test_step_block():
    g = [[0, 0, 0, 0], [0, 1, 1, 0], [0, 1, 1, 0], [0, 0, 0, 0]]
    assert step(g) == _oracle.step(g)


def test_step_l_shape():
    g = [[1, 0, 0], [1, 0, 0], [1, 1, 0]]
    assert step(g) == _oracle.step(g)


def test_step_full_row():
    g = [[1, 1, 1, 1, 1], [0, 0, 0, 0, 0], [0, 0, 0, 0, 0]]
    assert step(g) == _oracle.step(g)


def test_step_checkerboard():
    g = [[1, 0, 1, 0], [0, 1, 0, 1], [1, 0, 1, 0], [0, 1, 0, 1]]
    assert step(g) == _oracle.step(g)


def test_step_corners():
    g = [[1, 0, 1], [0, 0, 0], [1, 0, 1]]
    assert step(g) == _oracle.step(g)


def test_step_dense_5x5():
    g = [
        [0, 1, 1, 0, 0],
        [1, 1, 0, 1, 0],
        [0, 0, 1, 1, 1],
        [1, 0, 0, 1, 0],
        [0, 1, 0, 0, 1],
    ]
    assert step(g) == _oracle.step(g)


# ── multi-step (tests run + wrapping behavior) ──

def test_run_2_steps():
    g = [[0, 1, 0], [0, 1, 0], [0, 1, 0]]
    assert run(g, 2) == _oracle.run(g, 2)


def test_run_10_steps():
    g = [[1, 0, 0, 0], [0, 1, 1, 0], [0, 1, 0, 0], [0, 0, 0, 1]]
    assert run(g, 10) == _oracle.run(g, 10)


def test_run_50_steps():
    g = [
        [0, 0, 1, 0, 0, 0],
        [0, 0, 0, 1, 0, 0],
        [0, 1, 1, 1, 0, 0],
        [0, 0, 0, 0, 0, 0],
        [0, 0, 0, 0, 0, 0],
        [0, 0, 0, 0, 0, 0],
    ]
    assert run(g, 50) == _oracle.run(g, 50)


# ── population ──

def test_population_empty():
    assert population([[0, 0], [0, 0]]) == 0


def test_population_full():
    assert population([[1, 1], [1, 1]]) == 4


def test_population_mixed():
    g = [[1, 0, 1], [0, 1, 0]]
    assert population(g) == _oracle.population(g)


# ── period detection ──

def test_period_still_life():
    # A 2x2 block in a 4x4 grid is a still life
    g = [[0, 0, 0, 0], [0, 1, 1, 0], [0, 1, 1, 0], [0, 0, 0, 0]]
    assert period(g) == _oracle.period(g)


def test_period_oscillator():
    g = [[0, 0, 0, 0, 0], [0, 0, 1, 0, 0], [0, 0, 1, 0, 0], [0, 0, 1, 0, 0], [0, 0, 0, 0, 0]]
    p = period(g)
    p_expected = _oracle.period(g)
    assert p == p_expected, f"got period {p}, expected {p_expected}"


def test_period_all_dead():
    g = [[0, 0, 0], [0, 0, 0], [0, 0, 0]]
    assert period(g) == _oracle.period(g)


def test_period_complex():
    g = [
        [0, 0, 1, 0, 0, 0],
        [0, 0, 0, 1, 0, 0],
        [0, 1, 1, 1, 0, 0],
        [0, 0, 0, 0, 0, 0],
        [0, 0, 0, 0, 0, 0],
        [0, 0, 0, 0, 0, 0],
    ]
    p = period(g, max_steps=500)
    p_expected = _oracle.period(g, max_steps=500)
    assert p == p_expected, f"got period {p}, expected {p_expected}"


if __name__ == "__main__":
    test_step_empty()
    test_step_all_dead()
    test_step_single_cell()
    test_step_vertical_line_3x3()
    test_step_horizontal_line_3x3()
    test_step_block()
    test_step_l_shape()
    test_step_full_row()
    test_step_checkerboard()
    test_step_corners()
    test_step_dense_5x5()
    test_run_2_steps()
    test_run_10_steps()
    test_run_50_steps()
    test_population_empty()
    test_population_full()
    test_population_mixed()
    test_period_still_life()
    test_period_oscillator()
    test_period_all_dead()
    test_period_complex()
    print("All tests passed")
