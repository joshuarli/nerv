"""Reference implementation."""


class Package:
    def __init__(self, name, version, dependencies=None):
        self.name = name
        self.version = version
        self.dependencies = dependencies or []

    def __repr__(self):
        return f"Package({self.name!r}, {self.version!r})"

    def __eq__(self, other):
        return isinstance(other, Package) and self.name == other.name and self.version == other.version

    def __hash__(self):
        return hash((self.name, self.version))


class Dependency:
    def __init__(self, name, min_version=None, max_version=None):
        self.name = name
        self.min_version = min_version
        self.max_version = max_version


class ResolverError(Exception):
    pass


class CircularDependencyError(ResolverError):
    pass


class NoMatchingVersionError(ResolverError):
    pass


class Resolver:
    def __init__(self):
        self._packages = {}  # name -> [Package]

    def add_package(self, package):
        self._packages.setdefault(package.name, []).append(package)

    def find_matching(self, dependency):
        candidates = self._packages.get(dependency.name, [])
        filtered = []
        for p in candidates:
            if dependency.min_version and p.version < dependency.min_version:
                continue
            if dependency.max_version and p.version >= dependency.max_version:
                continue
            filtered.append(p)
        if not filtered:
            raise NoMatchingVersionError(f"No match for {dependency}")
        filtered.sort(key=lambda p: p.version, reverse=True)
        return filtered[0]

    def resolve(self, root_packages):
        resolved = set()
        visiting = set()

        def visit(name, dep=None):
            pkg = self.find_matching(dep or Dependency(name))
            if pkg.name in visiting:
                raise CircularDependencyError(f"Circular: {name}")
            if pkg in resolved:
                return
            visiting.add(pkg.name)
            for d in pkg.dependencies:
                visit(d.name, d)
            visiting.remove(pkg.name)
            resolved.add(pkg)

        for name in root_packages:
            visit(name)
        return resolved

    def install_order(self, root_packages):
        order = []
        visited = set()
        visiting = set()

        def visit(name, dep=None):
            pkg = self.find_matching(dep or Dependency(name))
            if pkg.name in visiting:
                raise CircularDependencyError(f"Circular: {name}")
            if pkg in visited:
                return
            visiting.add(pkg.name)
            for d in pkg.dependencies:
                visit(d.name, d)
            visiting.remove(pkg.name)
            visited.add(pkg)
            order.append(pkg)

        for name in root_packages:
            visit(name)
        return order
