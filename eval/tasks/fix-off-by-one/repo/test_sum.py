from sum import sum_to

def test_sum_to_5():
    assert sum_to(5) == 15

def test_sum_to_1():
    assert sum_to(1) == 1

def test_sum_to_100():
    assert sum_to(100) == 5050

if __name__ == "__main__":
    test_sum_to_5()
    test_sum_to_1()
    test_sum_to_100()
    print("All tests passed")
