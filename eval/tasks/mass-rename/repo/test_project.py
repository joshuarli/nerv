"""Tests — all use retrieve_data (the new name) instead of fetch_data."""

import unittest


class TestCore(unittest.TestCase):
    def test_retrieve_data_basic(self):
        from core import retrieve_data
        result = retrieve_data("api")
        self.assertEqual(result["source"], "api")
        self.assertIn("data", result)

    def test_retrieve_data_timeout(self):
        from core import retrieve_data
        result = retrieve_data("api", timeout=10)
        self.assertEqual(result["timeout"], 10)

    def test_retrieve_data_empty_source_raises(self):
        from core import retrieve_data
        with self.assertRaises(ValueError):
            retrieve_data("")

    def test_retrieve_data_batch(self):
        from core import retrieve_data_batch
        results = retrieve_data_batch(["a", "b", "c"])
        self.assertEqual(len(results), 3)

    def test_retrieve_data_cached(self):
        from core import retrieve_data_cached
        cache = {}
        r1 = retrieve_data_cached("x", cache)
        r2 = retrieve_data_cached("x", cache)
        self.assertEqual(r1, r2)


class TestPipeline(unittest.TestCase):
    def test_pipeline_run(self):
        from pipeline import Pipeline
        p = Pipeline(["s1", "s2"])
        results = p.run()
        self.assertEqual(len(results), 2)

    def test_pipeline_single(self):
        from pipeline import Pipeline
        p = Pipeline([])
        result = p.run_single("s1")
        self.assertEqual(result["source"], "s1")


class TestCache(unittest.TestCase):
    def test_cache_get(self):
        from cache import DataCache
        c = DataCache()
        r = c.get("src1")
        self.assertEqual(r["source"], "src1")

    def test_cache_fresh(self):
        from cache import DataCache
        c = DataCache()
        c.get("src1")
        r = c.get_fresh("src1")
        self.assertEqual(r["source"], "src1")


class TestValidator(unittest.TestCase):
    def test_validate_source(self):
        from validator import validate_source
        result = validate_source("api")
        self.assertIn("data", result)

    def test_validate_sources(self):
        from validator import validate_sources
        results = validate_sources(["a", "b"])
        self.assertEqual(len(results), 2)


class TestMonitor(unittest.TestCase):
    def test_monitored_fetch(self):
        from monitor import Monitor
        m = Monitor()
        r = m.retrieve_data_monitored("api")
        self.assertEqual(r["source"], "api")
        self.assertEqual(m.call_count, 1)

    def test_monitored_batch(self):
        from monitor import Monitor
        m = Monitor()
        results = m.retrieve_data_batch_monitored(["a", "b"])
        self.assertEqual(len(results), 2)


class TestRetry(unittest.TestCase):
    def test_retrieve_with_retry(self):
        from retry import retrieve_data_with_retry
        result = retrieve_data_with_retry("api")
        self.assertEqual(result["source"], "api")

    def test_retrieve_or_default(self):
        from retry import retrieve_data_or_default
        result = retrieve_data_or_default("api")
        self.assertIsNotNone(result)

    def test_retrieve_or_default_empty(self):
        from retry import retrieve_data_or_default
        result = retrieve_data_or_default("", default={"fallback": True})
        self.assertEqual(result, {"fallback": True})


class TestCli(unittest.TestCase):
    def test_cli_single(self):
        from cli import run_cli
        code = run_cli(["src"])
        self.assertEqual(code, 0)


class TestExporter(unittest.TestCase):
    def test_export_json(self):
        import json
        from exporter import export_json
        result = json.loads(export_json("api"))
        self.assertEqual(result["source"], "api")

    def test_export_csv(self):
        from exporter import export_csv
        csv = export_csv(["a", "b"])
        lines = csv.strip().split("\n")
        self.assertEqual(len(lines), 3)  # header + 2 rows


if __name__ == "__main__":
    unittest.main()
