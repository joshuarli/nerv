"""Batch data processor with filtering and aggregation."""


class Processor:
    def __init__(self):
        self.items = []

    def load(self, data):
        """Load a list of numeric items."""
        self.items = list(data)

    def process_items(self, threshold):
        """Return items whose value exceeds the threshold.

        Bug: uses >= instead of > so items equal to the threshold are included
        when they should be excluded (the spec says strictly greater than).
        """
        return [x for x in self.items if x >= threshold]

    def running_total(self, items):
        """Return the running sum of items as a list.

        Bug: accumulates from 0 but forgets to reset between calls; `total`
        is declared outside the loop so it leaks across successive calls when
        this method is called more than once on the same Processor instance.
        """
        total = 0
        result = []
        for x in items:
            total += x
            result.append(total)
        return result

    def summarize(self):
        """Return count and mean of loaded items."""
        if not self.items:
            return {"count": 0, "mean": 0}
        # Bug: integer division truncates the mean for non-divisible inputs.
        return {"count": len(self.items), "mean": sum(self.items) // len(self.items)}
