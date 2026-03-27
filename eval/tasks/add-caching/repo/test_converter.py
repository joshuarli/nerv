from converter import TemperatureConverter


def test_basic_conversions():
    tc = TemperatureConverter()
    assert tc.celsius_to_fahrenheit(0) == 32
    assert tc.celsius_to_fahrenheit(100) == 212
    assert abs(tc.fahrenheit_to_celsius(32) - 0) < 0.01
    assert tc.celsius_to_kelvin(0) == 273.15
    assert abs(tc.kelvin_to_celsius(273.15) - 0) < 0.01


def test_history():
    tc = TemperatureConverter()
    tc.celsius_to_fahrenheit(100)
    tc.fahrenheit_to_celsius(32)
    h = tc.get_history()
    assert len(h) == 2
    assert h[0][0] == "c_to_f"
    tc.clear_history()
    assert len(tc.get_history()) == 0


def test_cache_returns_same_result():
    """Repeated conversions with the same input should return cached results."""
    tc = TemperatureConverter()
    r1 = tc.celsius_to_fahrenheit(100)
    r2 = tc.celsius_to_fahrenheit(100)
    assert r1 == r2


def test_cache_avoids_duplicate_history():
    """Cached results should NOT add duplicate entries to history."""
    tc = TemperatureConverter()
    tc.celsius_to_fahrenheit(100)
    tc.celsius_to_fahrenheit(100)  # cached — should not add to history
    tc.celsius_to_fahrenheit(0)    # different input — should add
    h = tc.get_history()
    assert len(h) == 2, f"expected 2 history entries, got {len(h)}: {h}"


def test_cache_works_across_methods():
    """Each conversion method should have its own cache."""
    tc = TemperatureConverter()
    tc.celsius_to_fahrenheit(100)
    tc.celsius_to_kelvin(100)  # different method, same input — not cached
    h = tc.get_history()
    assert len(h) == 2


def test_cache_cleared_with_history():
    """clear_history() should also clear the cache."""
    tc = TemperatureConverter()
    tc.celsius_to_fahrenheit(100)
    tc.clear_history()
    tc.celsius_to_fahrenheit(100)  # should recompute, not be cached
    h = tc.get_history()
    assert len(h) == 1
    assert h[0][0] == "c_to_f"


if __name__ == "__main__":
    test_basic_conversions()
    test_history()
    test_cache_returns_same_result()
    test_cache_avoids_duplicate_history()
    test_cache_works_across_methods()
    test_cache_cleared_with_history()
    print("All tests passed")
