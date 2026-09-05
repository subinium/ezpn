import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("evidence", Path(__file__).with_name("check-evidence.py"))
evidence = importlib.util.module_from_spec(spec)
spec.loader.exec_module(evidence)


class EvidenceTests(unittest.TestCase):
    def test_empty_or_ignored_tests_fail(self):
        for text in ["", "test result: ok. 0 passed; 0 failed; 6 ignored; 0 measured; 0 filtered out", "test result: ok. 1 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out"]:
            with self.assertRaises(ValueError):
                evidence.test_counts(text)

    def test_counts_are_actual_not_a_hardcoded_total(self):
        text = "test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n"
        self.assertEqual(evidence.test_counts(text * 2)["passed"], 24)

    def test_missing_module_coverage_fails(self):
        with self.assertRaises(ValueError):
            evidence.coverage({"data": [{"files": []}]}, Path("/repo"))

    def test_coverage_uses_lines_not_regions(self):
        files = [{"filename": f"/repo/src/{name}.rs", "summary": {"lines": {"count": 100, "covered": 60}, "regions": {"percent": 99}}} for name in ["protocol", "layout", "workspace"]]
        with self.assertRaises(ValueError):
            evidence.coverage({"data": [{"files": files}]}, Path("/repo"))

    def test_regression_is_per_metric_with_real_five_percent_floor(self):
        def run(bound):
            return {"codec": {"mean": {"confidence_interval": {"lower_bound": bound}}}}
        evidence.regression([run(.04), run(.04), run(.04)])
        with self.assertRaises(ValueError):
            evidence.regression([run(.06), run(.06), run(0)])
        with self.assertRaises(ValueError):
            evidence.regression([{}, {}, {}])
        with self.assertRaises(ValueError):
            evidence.regression([run(0)])


if __name__ == "__main__":
    unittest.main()
