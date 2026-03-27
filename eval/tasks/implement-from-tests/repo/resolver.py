"""Dependency resolver."""


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

    def __repr__(self):
        parts = [self.name]
        if self.min_version:
            parts.append(f">={self.min_version}")
        if self.max_version:
            parts.append(f"<{self.max_version}")
        return f"Dependency({', '.join(parts)})"


class ResolverError(Exception):
    pass


class CircularDependencyError(ResolverError):
    pass


class NoMatchingVersionError(ResolverError):
    pass


class Resolver:
    def __init__(self):
        raise NotImplementedError

    def add_package(self, package):
        raise NotImplementedError

    def find_matching(self, dependency):
        raise NotImplementedError

    def resolve(self, root_packages):
        raise NotImplementedError

    def install_order(self, root_packages):
        raise NotImplementedError
