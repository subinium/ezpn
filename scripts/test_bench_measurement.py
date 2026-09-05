import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest import mock

spec = importlib.util.spec_from_file_location("symmetric_bench", Path(__file__).with_name("bench-regression.py"))
bench = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bench)


class SymmetricMeasurementTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="ezpn-symmetric-test-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        compiler = mock.patch.object(bench, "compiler_identity", return_value="rustc fixture verbose\n")
        compiler.start()
        self.addCleanup(compiler.stop)
        self.source = self.root / "source"
        (self.source / "benches").mkdir(parents=True)
        (self.source / "Cargo.lock").write_text("fixture lock\n")
        (self.source / "Cargo.toml").write_text("fixture manifest\n")
        for name in bench.BENCHES:
            (self.source / "benches" / f"{name}.rs").write_text(f"fixture {name}\n")
        self.out = self.root / "evidence"
        self.out.mkdir()
        self.target = self.out / "build"
        self.metrics = {name: [f"{name}/metric-{i}" for i in range(count)]
                        for name, count in zip(bench.BENCHES, [6, 8, 4, 4])}
        self.calls = []
        self.base = "b" * 40

    def write_metric(self, root, metric, sides=("main", "new")):
        data = {
            "benchmark.json": {"directory_name": metric, "full_id": metric},
            "sample.json": {"sampling_mode": "Linear", "iters": list(range(1, 11)),
                            "times": list(range(100, 1100, 100))},
            "estimates.json": {"mean": {"point_estimate": 100}},
            "tukey.json": [90, 95, 105, 110],
        }
        for side in sides:
            directory = root / metric / side
            directory.mkdir(parents=True, exist_ok=True)
            for name, value in data.items():
                (directory / name).write_text(json.dumps(value))

    def fake_subprocess(self, command, **kwargs):
        self.calls.append(command)
        if command[:2] == ["cargo", "bench"]:
            name = command[command.index("--bench") + 1]
            self.assertEqual(command[command.index("--") + 1:], ["--save-baseline", "main"])
            criterion = Path(kwargs["env"]["CRITERION_HOME"])
            self.assertEqual(criterion, self.out / "criterion-current")
            self.assertNotIn("CARGO_CRITERION_PORT", kwargs["env"])
            if name == bench.BENCHES[0]:
                self.assertEqual(list(criterion.iterdir()), [], "every measurement starts empty")
            for metric in self.metrics[name]:
                self.write_metric(criterion, metric)
            executable = Path(kwargs["env"]["CARGO_TARGET_DIR"]) / "release/deps" / name
            executable.parent.mkdir(parents=True, exist_ok=True)
            executable.write_bytes(b"mock executable " + name.encode())
            executable.chmod(0o755)
            kwargs["stdout"].write(json.dumps({
                "reason": "compiler-artifact", "executable": str(executable),
                "target": {"name": name, "kind": ["bench"]}, "profile": {"opt_level": "3"},
                "features": ["default", "render-diff"], "fresh": False,
            }) + "\n")
        elif "--load-baseline" in command:
            name = Path(command[0]).name
            self.assertEqual(command[1:5], ["--bench", "--baseline", "main", "--load-baseline"])
            self.assertNotIn("CARGO_CRITERION_PORT", kwargs["env"])
            criterion = Path(kwargs["env"]["CRITERION_HOME"])
            for metric in self.metrics[name]:
                change = criterion / metric / "change"
                change.mkdir()
                (change / "estimates.json").write_text(json.dumps({
                    "mean": {"confidence_interval": {"lower_bound": 0.01}},
                }))
        else:
            self.fail(f"unexpected command: {command}")
        return subprocess.CompletedProcess(command, 0)

    def measure(self, label="baseline"):
        with mock.patch.object(bench.subprocess, "run", side_effect=self.fake_subprocess):
            return bench.measure(self.source, self.target, self.out, label, self.base)

    def prepare_analysis(self):
        baseline = self.measure()
        candidate = self.measure("candidate-1")
        roots = (self.out / "raw/baseline", self.out / "raw/candidate-1")
        hashes = {"baseline": baseline["raw_sha256"], "candidate": candidate["raw_sha256"]}
        return roots, candidate["executables"], hashes

    def analyze(self, roots, executables, hashes):
        return bench.offline_analysis(*roots, executables, self.out, "candidate-1", {}, hashes)

    def test_all_measurements_use_same_empty_path_and_save_arguments(self):
        with mock.patch.dict(os.environ, {"CRITERION_HOME": "/unrelated", "CARGO_CRITERION_PORT": "12345"}):
            for label in ["baseline", "candidate-1", "candidate-2", "candidate-3"]:
                manifest = self.measure(label)
                self.assertEqual(manifest["status"], "PASS")
                self.assertEqual(manifest["mode"], bench.MEASUREMENT_MODE)
                self.assertEqual(len(manifest["metrics"]), 22)
                self.assertEqual(manifest["raw_sha256"], bench.tree_hashes(self.out / "raw" / label))
                self.assertEqual(set(manifest["executables"]), set(bench.BENCHES))
                self.assertFalse((self.out / "criterion-current").exists())
        self.assertEqual(len(self.calls), 16, "no capture-triggered recompilation")

    def test_all_four_files_are_archived_for_new_and_main(self):
        self.measure()
        for side in ("new", "main"):
            directory = self.out / "raw/baseline" / self.metrics[bench.BENCHES[0]][0] / side
            self.assertEqual({p.name for p in directory.iterdir()}, set(bench.SAVED_FILES))

    def test_optional_raw_csv_is_preserved_without_requiring_it(self):
        def with_csv(command, **kwargs):
            result = self.fake_subprocess(command, **kwargs)
            for path in Path(kwargs["env"]["CRITERION_HOME"]).glob("**/sample.json"):
                path.with_name("raw.csv").write_text("original raw bytes\n")
            return result
        with mock.patch.object(bench.subprocess, "run", side_effect=with_csv):
            manifest = bench.measure(self.source, self.target, self.out, "baseline", self.base)
        self.assertEqual(sum(name.endswith("raw.csv") for name in manifest["raw_sha256"]), 44)

    def test_command_failure_keeps_partial_raw_and_original_error(self):
        failure = subprocess.CalledProcessError(42, ["cargo", "bench"])
        def fail(command, **kwargs):
            self.fake_subprocess(command, **kwargs)
            raise failure
        with mock.patch.object(bench.subprocess, "run", side_effect=fail):
            with self.assertRaises(subprocess.CalledProcessError) as raised:
                bench.measure(self.source, self.target, self.out, "baseline", self.base)
        self.assertIs(raised.exception, failure)
        self.assertTrue(list((self.out / "raw/baseline").glob("**/sample.json")))
        manifest = json.loads((self.out / "measurement-baseline.json").read_text())
        self.assertEqual(manifest["status"], "FAIL")
        self.assertEqual(manifest["exit_code"], 42)
        self.assertEqual(len(manifest["commands"]), 1)

    def test_missing_required_file_fails_and_retains_partial_data(self):
        def missing(command, **kwargs):
            result = self.fake_subprocess(command, **kwargs)
            if command[command.index("--bench") + 1] == bench.BENCHES[-1]:
                next(Path(kwargs["env"]["CRITERION_HOME"]).glob("**/main/tukey.json")).unlink()
            return result
        with mock.patch.object(bench.subprocess, "run", side_effect=missing):
            with self.assertRaisesRegex(ValueError, "missing/invalid"):
                bench.measure(self.source, self.target, self.out, "baseline", self.base)
        self.assertTrue((self.out / "raw/baseline").is_dir())

    def test_measurement_requires_exact_22_metrics(self):
        self.metrics[bench.BENCHES[-1]].pop()
        with self.assertRaisesRegex(ValueError, "expected 22"):
            self.measure()

    def test_existing_output_and_repeated_measurement_never_delete_evidence(self):
        self.measure()
        before = bench.tree_hashes(self.out)
        with self.assertRaisesRegex(ValueError, "fresh --out"):
            bench.prepare_output(self.out, False, self.base)
        with self.assertRaisesRegex(ValueError, "already exists"):
            self.measure()
        self.assertEqual(before, bench.tree_hashes(self.out))

    def test_legacy_reuse_is_rejected_without_writes(self):
        (self.out / "base.txt").write_text(self.base + "\n")
        self.write_metric(self.out / "base-target/criterion", "old/metric")
        before = bench.tree_hashes(self.out)
        with self.assertRaisesRegex(ValueError, "legacy/unproven"):
            bench.prepare_output(self.out, True, self.base)
        self.assertEqual(before, bench.tree_hashes(self.out))

    def test_completed_new_baseline_reuse_validates_raw_and_executable_hashes(self):
        self.measure()
        (self.out / "base.txt").write_text(self.base + "\n")
        bench.prepare_output(self.out, True, self.base)
        sample = next((self.out / "raw/baseline").glob("**/main/sample.json"))
        sample.write_text("modified")
        with self.assertRaisesRegex(ValueError, "raw evidence hash"):
            bench.prepare_output(self.out, True, self.base)

    def test_reuse_rejects_prior_candidate_partial_evidence(self):
        self.measure()
        (self.out / "base.txt").write_text(self.base + "\n")
        (self.out / "candidate-1-render_hotpaths.log").write_text("prior failure\n")
        before = bench.tree_hashes(self.out)
        with self.assertRaisesRegex(ValueError, "candidate evidence"):
            bench.prepare_output(self.out, True, self.base)
        self.assertEqual(before, bench.tree_hashes(self.out))

    def test_contract_covers_manifest_toolchain_flags_and_compiler_without_env_dump(self):
        before = bench.source_contract(self.source)
        (self.source / "Cargo.toml").write_text("changed manifest\n")
        (self.source / "rust-toolchain.toml").write_text("pinned toolchain\n")
        after = bench.source_contract(self.source)
        self.assertNotEqual(before["configuration_sha256"], after["configuration_sha256"])
        with mock.patch.dict(os.environ, {"RUSTFLAGS": "-C opt-level=2", "UNRELATED_SECRET": "must-not-record"}):
            flags = bench.source_contract(self.source)
        self.assertEqual(flags["compiler_environment"]["RUSTFLAGS"], "-C opt-level=2")
        self.assertNotIn("must-not-record", json.dumps(flags))
        self.assertEqual(flags["rustc_verbose"], "rustc fixture verbose\n")
        with mock.patch.object(bench, "compiler_identity", return_value="different rustc\n"):
            self.assertNotEqual(flags["rustc_verbose"], bench.source_contract(self.source)["rustc_verbose"])

    def test_offline_uses_saved_executables_and_preserves_all_inputs(self):
        roots, executables, hashes = self.prepare_analysis()
        with mock.patch.object(bench.subprocess, "run", side_effect=self.fake_subprocess):
            estimates = self.analyze(roots, executables, hashes)
        self.assertEqual(len(estimates), 22)
        record = json.loads((self.out / "analysis/candidate-1/status.json").read_text())
        self.assertEqual(record["status"], "PASS")
        self.assertEqual(record["execution_kind"], "unmeasured-analysis")
        self.assertEqual(record["input_sha256"], record["input_sha256_after"])
        self.assertEqual(record["staged_input_sha256"], record["staged_input_sha256_after"])
        self.assertTrue(all(command["execution_kind"] == "unmeasured-analysis" for command in record["commands"]))
        for path in self.out.glob("protocol-codec-*.json"):
            self.assertEqual(json.loads(path.read_text())["execution_kind"], "benchmark")

    def test_offline_rejects_sample_changed_since_measurement(self):
        roots, executables, hashes = self.prepare_analysis()
        next(roots[0].glob("**/sample.json")).write_text("changed")
        with mock.patch.object(bench.subprocess, "run") as run:
            with self.assertRaisesRegex(ValueError, "since measurement"):
                self.analyze(roots, executables, hashes)
        run.assert_not_called()

    def test_offline_rejects_executable_changed_since_measurement(self):
        roots, executables, hashes = self.prepare_analysis()
        (self.out / executables[bench.BENCHES[0]]["path"]).write_bytes(b"changed executable")
        with mock.patch.object(bench.subprocess, "run") as run:
            with self.assertRaisesRegex(ValueError, "differs from measured"):
                self.analyze(roots, executables, hashes)
        run.assert_not_called()

    def test_offline_detects_mutation_of_original_or_staged_samples(self):
        for location in ("original", "staged"):
            with self.subTest(location=location):
                roots, executables, hashes = self.prepare_analysis()
                def mutate(command, **kwargs):
                    result = self.fake_subprocess(command, **kwargs)
                    directory = roots[1] if location == "original" else Path(kwargs["env"]["CRITERION_HOME"])
                    sample = next(directory.glob("**/main/sample.json"))
                    sample.write_text(sample.read_text() + " ")
                    return result
                with mock.patch.object(bench.subprocess, "run", side_effect=mutate):
                    with self.assertRaisesRegex(ValueError, "changed during offline"):
                        self.analyze(roots, executables, hashes)
                self.out = self.root / f"next-evidence-{location}"
                self.out.mkdir()
                self.target = self.out / "build"

    def test_offline_failure_and_timeout_remain_failures_with_raw_data(self):
        roots, executables, hashes = self.prepare_analysis()
        failure = subprocess.TimeoutExpired(["fixture"], 600)
        with mock.patch.object(bench.subprocess, "run", side_effect=failure):
            with self.assertRaises(subprocess.TimeoutExpired) as raised:
                self.analyze(roots, executables, hashes)
        self.assertIs(raised.exception, failure)
        record = json.loads((self.out / "analysis/candidate-1/status.json").read_text())
        self.assertEqual(record["status"], "FAIL")
        self.assertTrue(record["timed_out"])
        self.assertEqual(record["input_sha256"], record["input_sha256_after"])

    def test_offline_nonzero_exit_preserves_status_and_does_not_continue(self):
        roots, executables, hashes = self.prepare_analysis()
        with mock.patch.object(bench.subprocess, "run", return_value=subprocess.CompletedProcess(["fixture"], 17)) as run:
            with self.assertRaises(subprocess.CalledProcessError) as raised:
                self.analyze(roots, executables, hashes)
        self.assertEqual(raised.exception.returncode, 17)
        self.assertEqual(run.call_count, 1)
        record = json.loads((self.out / "analysis/candidate-1/status.json").read_text())
        self.assertEqual(record["commands"][0]["exit_code"], 17)
        self.assertEqual(record["status"], "FAIL")

    def test_offline_missing_change_estimates_fails_closed(self):
        roots, executables, hashes = self.prepare_analysis()
        with mock.patch.object(bench.subprocess, "run", return_value=subprocess.CompletedProcess([], 0)):
            with self.assertRaisesRegex(ValueError, "offline Criterion comparison metrics"):
                self.analyze(roots, executables, hashes)

    def test_invalid_samples_and_symlinks_are_rejected(self):
        root = self.out / "raw"
        self.write_metric(root, "one")
        path = root / "one/main/sample.json"
        for values in ([], [float("nan")], [-1], [True]):
            with self.subTest(values=values):
                path.write_text(json.dumps({"sampling_mode": "Linear", "iters": values, "times": values}))
                with self.assertRaisesRegex(ValueError, "invalid Criterion sample"):
                    bench.saved_metrics(root)
        path.unlink()
        path.symlink_to(root / "one/new/sample.json")
        with self.assertRaisesRegex(ValueError, "symlink"):
            bench.tree_hashes(root)

    def test_main_completes_all_timing_before_offline_analysis(self):
        fake_root = self.root / "repo"
        shutil.copytree(self.source, fake_root)
        (fake_root / "scripts").mkdir()
        tracked = ["Cargo.lock", "Cargo.toml", *[f"benches/{name}.rs" for name in bench.BENCHES]]
        events = []
        def check_output(command, **kwargs):
            if "ls-files" in command:
                return b"\0".join(path.encode() for path in tracked) + b"\0"
            if "diff" in command:
                return b""
            return self.base + "\n"
        def run(command, **kwargs):
            if command[:2] == ["git", "archive"]:
                return subprocess.CompletedProcess(command, 0)
            if command[:2] == ["tar", "xf"]:
                destination = Path(command[-1])
                shutil.copytree(self.source, destination, dirs_exist_ok=True)
                return subprocess.CompletedProcess(command, 0)
            if command[:2] == ["cargo", "bench"]:
                events.append("measure")
                return self.fake_subprocess(command, **kwargs)
            if "--load-baseline" in command:
                events.append("analysis")
                return self.fake_subprocess(command, **kwargs)
            self.assertEqual(command[2], "bench")
            self.assertEqual(len(command[3:]), 3)
            return subprocess.CompletedProcess(command, 0, stdout="fixture gate\n", stderr="")
        with mock.patch.object(bench, "__file__", str(fake_root / "scripts/bench-regression.py")), \
                mock.patch.object(bench.sys, "argv", ["bench", "--base", self.base, "--out", str(self.out)]), \
                mock.patch.object(bench.subprocess, "check_output", side_effect=check_output), \
                mock.patch.object(bench.subprocess, "run", side_effect=run):
            bench.main()
        self.assertEqual(events, ["measure"] * 16 + ["analysis"] * 12)
        status = json.loads((self.out / "status.json").read_text())
        self.assertEqual((status["runs"], status["compared_metrics"], status["threshold_percent"]), (3, 22, 5))


if __name__ == "__main__":
    unittest.main()
