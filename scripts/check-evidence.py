#!/usr/bin/env python3
"""Fail-closed parsers for libtest, LLVM JSON and Criterion JSON evidence."""
import argparse
import json
import math
from pathlib import Path
import re


def test_counts(text):
    rows = re.findall(r"test result: (\w+)\. (\d+) passed; (\d+) failed; (\d+) ignored; (\d+) measured; (\d+) filtered out", text)
    if not rows:
        raise ValueError("no libtest result summaries found")
    counts = {key: sum(int(row[i + 1]) for row in rows) for i, key in enumerate(["passed", "failed", "ignored", "measured", "filtered"])}
    if any(row[0] != "ok" for row in rows) or not counts["passed"] or counts["failed"] or counts["ignored"] or counts["filtered"]:
        raise ValueError(f"incomplete or failing test execution: {counts}")
    return counts


def coverage(data, root, floor=65, module_floor=70):
    files = [entry for unit in data["data"] for entry in unit["files"]
             if Path(entry["filename"]).resolve().is_relative_to(root / "src")]
    if not files:
        raise ValueError("no src coverage records")
    total = sum(entry["summary"]["lines"]["count"] for entry in files)
    covered = sum(entry["summary"]["lines"]["covered"] for entry in files)
    if total <= 0:
        raise ValueError("zero instrumented source lines")
    values = {"overall": covered * 100 / total}
    for module in ("protocol", "layout", "workspace"):
        records = [f for f in files if Path(f["filename"]).resolve() == root / "src" / f"{module}.rs"]
        if len(records) != 1 or records[0]["summary"]["lines"]["count"] <= 0:
            raise ValueError(f"missing/ambiguous coverage for {module}")
        lines = records[0]["summary"]["lines"]
        values[module] = lines["covered"] * 100 / lines["count"]
    if values["overall"] < floor or any(values[key] < module_floor for key in values if key != "overall"):
        raise ValueError(f"line coverage below {floor}% overall / {module_floor}% module floors: {values}")
    return values


def regression(runs, threshold=0.05):
    if len(runs) != 3:
        raise ValueError("exactly three Criterion comparison runs are required")
    keys = set(runs[0])
    if not keys or any(set(run) != keys for run in runs):
        raise ValueError("missing or inconsistent Criterion comparisons")
    bad = []
    for key in sorted(keys):
        bounds = [run[key]["mean"]["confidence_interval"]["lower_bound"] for run in runs]
        if not all(math.isfinite(bound) for bound in bounds):
            raise ValueError(f"nonfinite Criterion estimate: {key}")
        if sum(bound > threshold for bound in bounds) >= 2:
            bad.append(key)
    if bad:
        raise ValueError(f"significant >{threshold * 100:g}% regression in at least 2/3 runs: {bad}")
    return {"compared_metrics": len(keys), "threshold_percent": threshold * 100}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("kind", choices=["tests", "coverage", "bench"])
    parser.add_argument("files", nargs="+", type=Path)
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    if args.kind == "tests":
        result = test_counts(args.files[0].read_text())
    elif args.kind == "coverage":
        result = coverage(json.loads(args.files[0].read_text()), root)
    else:
        result = regression([json.loads(path.read_text()) for path in args.files])
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
