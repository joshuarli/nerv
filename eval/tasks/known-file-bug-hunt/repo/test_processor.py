from processor import Processor


def test_process_items_excludes_threshold():
    p = Processor()
    p.load([1, 2, 3, 4, 5])
    result = p.process_items(3)
    assert 3 not in result, f"threshold value 3 should be excluded, got: {result}"
    assert result == [4, 5], f"expected [4, 5], got: {result}"


def test_summarize_mean_is_float():
    p = Processor()
    p.load([1, 2, 3])
    s = p.summarize()
    # 1+2+3=6, 6/3=2.0 — also works with integer division, so check non-divisible case.
    p.load([1, 2])
    s = p.summarize()
    assert abs(s["mean"] - 1.5) < 1e-9, f"mean of [1,2] should be 1.5, got {s['mean']}"


def test_process_items_all_above():
    p = Processor()
    p.load([10, 20, 30])
    result = p.process_items(5)
    assert result == [10, 20, 30], f"all items above threshold, got: {result}"


def test_process_items_none_above():
    p = Processor()
    p.load([1, 2, 3])
    result = p.process_items(10)
    assert result == [], f"no items above threshold, got: {result}"


if __name__ == "__main__":
    test_process_items_excludes_threshold()
    test_summarize_mean_is_float()
    test_process_items_all_above()
    test_process_items_none_above()
    print("All tests passed")
