from parse_sales import parse_sales, top_region

SAMPLE_CSV = """\
date,region,product,quantity,unit_price
2024-01-01,North,Widget,10,25
2024-01-01,South,Widget,5,25
2024-01-02,North,Gadget,3,50
2024-01-02,South,Gadget,8,50
2024-01-03,North,Widget,7,25
"""


def test_region_totals():
    totals = parse_sales(SAMPLE_CSV)
    # North: 10*25 + 3*50 + 7*25 = 250 + 150 + 175 = 575
    assert totals["North"] == 575, f"North={totals['North']}"
    # South: 5*25 + 8*50 = 125 + 400 = 525
    assert totals["South"] == 525, f"South={totals['South']}"


def test_top_region():
    region, total = top_region(SAMPLE_CSV)
    assert region == "North"
    assert total == 575


def test_single_row():
    csv = "date,region,product,quantity,unit_price\n2024-01-01,East,X,4,10\n"
    totals = parse_sales(csv)
    assert totals == {"East": 40}


if __name__ == "__main__":
    test_region_totals()
    test_top_region()
    test_single_row()
    print("All tests passed")
