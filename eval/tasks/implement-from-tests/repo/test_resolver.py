from resolver import (
    Package, Dependency, Resolver,
    CircularDependencyError, NoMatchingVersionError,
)


def make_repo(*packages):
    r = Resolver()
    for p in packages:
        r.add_package(p)
    return r


# ── find_matching ──

def test_find_matching_exact():
    r = make_repo(Package("foo", "1.0.0"))
    match = r.find_matching(Dependency("foo"))
    assert match == Package("foo", "1.0.0")


def test_find_matching_latest():
    r = make_repo(
        Package("foo", "1.0.0"),
        Package("foo", "2.0.0"),
        Package("foo", "1.5.0"),
    )
    match = r.find_matching(Dependency("foo"))
    assert match == Package("foo", "2.0.0"), f"got {match}"


def test_find_matching_min_version():
    r = make_repo(
        Package("foo", "1.0.0"),
        Package("foo", "2.0.0"),
        Package("foo", "3.0.0"),
    )
    match = r.find_matching(Dependency("foo", min_version="2.0.0"))
    assert match == Package("foo", "3.0.0"), f"got {match}"


def test_find_matching_max_version():
    r = make_repo(
        Package("foo", "1.0.0"),
        Package("foo", "2.0.0"),
        Package("foo", "3.0.0"),
    )
    match = r.find_matching(Dependency("foo", max_version="3.0.0"))
    assert match == Package("foo", "2.0.0"), f"got {match}"


def test_find_matching_range():
    r = make_repo(
        Package("foo", "1.0.0"),
        Package("foo", "2.0.0"),
        Package("foo", "2.5.0"),
        Package("foo", "3.0.0"),
    )
    match = r.find_matching(Dependency("foo", min_version="1.5.0", max_version="3.0.0"))
    assert match == Package("foo", "2.5.0"), f"got {match}"


def test_find_matching_not_found():
    r = make_repo(Package("foo", "1.0.0"))
    try:
        r.find_matching(Dependency("bar"))
        assert False, "should raise"
    except NoMatchingVersionError:
        pass


def test_find_matching_no_version_in_range():
    r = make_repo(Package("foo", "1.0.0"), Package("foo", "2.0.0"))
    try:
        r.find_matching(Dependency("foo", min_version="3.0.0"))
        assert False, "should raise"
    except NoMatchingVersionError:
        pass


# ── resolve ──

def test_resolve_no_deps():
    a = Package("a", "1.0.0")
    r = make_repo(a)
    resolved = r.resolve(["a"])
    assert resolved == {a}


def test_resolve_simple_chain():
    a = Package("a", "1.0.0", [Dependency("b")])
    b = Package("b", "1.0.0", [Dependency("c")])
    c = Package("c", "1.0.0")
    r = make_repo(a, b, c)
    resolved = r.resolve(["a"])
    assert resolved == {a, b, c}


def test_resolve_diamond():
    a = Package("a", "1.0.0", [Dependency("b"), Dependency("c")])
    b = Package("b", "1.0.0", [Dependency("d")])
    c = Package("c", "1.0.0", [Dependency("d")])
    d = Package("d", "1.0.0")
    r = make_repo(a, b, c, d)
    resolved = r.resolve(["a"])
    assert resolved == {a, b, c, d}


def test_resolve_version_constraint():
    a = Package("a", "1.0.0", [Dependency("b", min_version="2.0.0")])
    b1 = Package("b", "1.0.0")
    b2 = Package("b", "2.5.0")
    r = make_repo(a, b1, b2)
    resolved = r.resolve(["a"])
    assert Package("b", "2.5.0") in resolved
    assert Package("b", "1.0.0") not in resolved


def test_resolve_multiple_roots():
    a = Package("a", "1.0.0", [Dependency("c")])
    b = Package("b", "1.0.0", [Dependency("c")])
    c = Package("c", "1.0.0")
    r = make_repo(a, b, c)
    resolved = r.resolve(["a", "b"])
    assert resolved == {a, b, c}


def test_resolve_circular():
    a = Package("a", "1.0.0", [Dependency("b")])
    b = Package("b", "1.0.0", [Dependency("a")])
    r = make_repo(a, b)
    try:
        r.resolve(["a"])
        assert False, "should raise CircularDependencyError"
    except CircularDependencyError:
        pass


def test_resolve_self_circular():
    a = Package("a", "1.0.0", [Dependency("a")])
    r = make_repo(a)
    try:
        r.resolve(["a"])
        assert False, "should raise CircularDependencyError"
    except CircularDependencyError:
        pass


def test_resolve_missing_dep():
    a = Package("a", "1.0.0", [Dependency("missing")])
    r = make_repo(a)
    try:
        r.resolve(["a"])
        assert False, "should raise NoMatchingVersionError"
    except NoMatchingVersionError:
        pass


# ── install_order ──

def test_install_order_simple():
    a = Package("a", "1.0.0", [Dependency("b")])
    b = Package("b", "1.0.0")
    r = make_repo(a, b)
    order = r.install_order(["a"])
    assert order.index(b) < order.index(a), f"b should come before a: {order}"


def test_install_order_chain():
    a = Package("a", "1.0.0", [Dependency("b")])
    b = Package("b", "1.0.0", [Dependency("c")])
    c = Package("c", "1.0.0")
    r = make_repo(a, b, c)
    order = r.install_order(["a"])
    assert order.index(c) < order.index(b) < order.index(a), f"order: {order}"


def test_install_order_diamond():
    a = Package("a", "1.0.0", [Dependency("b"), Dependency("c")])
    b = Package("b", "1.0.0", [Dependency("d")])
    c = Package("c", "1.0.0", [Dependency("d")])
    d = Package("d", "1.0.0")
    r = make_repo(a, b, c, d)
    order = r.install_order(["a"])
    assert order.index(d) < order.index(b), f"d before b: {order}"
    assert order.index(d) < order.index(c), f"d before c: {order}"
    assert order.index(b) < order.index(a), f"b before a: {order}"
    assert order.index(c) < order.index(a), f"c before a: {order}"


def test_install_order_no_duplicates():
    a = Package("a", "1.0.0", [Dependency("c")])
    b = Package("b", "1.0.0", [Dependency("c")])
    c = Package("c", "1.0.0")
    r = make_repo(a, b, c)
    order = r.install_order(["a", "b"])
    names = [p.name for p in order]
    assert len(names) == len(set(names)), f"duplicates in order: {order}"


def test_install_order_circular():
    a = Package("a", "1.0.0", [Dependency("b")])
    b = Package("b", "1.0.0", [Dependency("a")])
    r = make_repo(a, b)
    try:
        r.install_order(["a"])
        assert False, "should raise CircularDependencyError"
    except CircularDependencyError:
        pass


if __name__ == "__main__":
    test_find_matching_exact()
    test_find_matching_latest()
    test_find_matching_min_version()
    test_find_matching_max_version()
    test_find_matching_range()
    test_find_matching_not_found()
    test_find_matching_no_version_in_range()
    test_resolve_no_deps()
    test_resolve_simple_chain()
    test_resolve_diamond()
    test_resolve_version_constraint()
    test_resolve_multiple_roots()
    test_resolve_circular()
    test_resolve_self_circular()
    test_resolve_missing_dep()
    test_install_order_simple()
    test_install_order_chain()
    test_install_order_diamond()
    test_install_order_no_duplicates()
    test_install_order_circular()
    print("All tests passed")
