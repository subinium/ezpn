import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock

spec = importlib.util.spec_from_file_location("bench", Path(__file__).with_name("bench-regression.py"))
bench = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bench)


class BenchArtifactTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="ezpn-artifact-test-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.target = self.root / "dedicated target"
        self.out = self.root / "evidence"
        self.out.mkdir()
        self.exe = self.make_executable(self.target / "release/deps/protocol_codec-current")
        self.log = self.out / "candidate-1-protocol_codec.log"
        self.command = ["cargo", "bench", "--locked", "--all-features", "--bench", "protocol_codec",
                        "--message-format=json-render-diagnostics", "--", "--baseline", "main"]

    def make_executable(self, path, content=b"\x7fELFfixture bytes"):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(content)
        path.chmod(0o755)
        return path

    def artifact(self, executable=None, **changes):
        message = {
            "reason": "compiler-artifact", "package_id": "path+file:///fixture#ezpn@0.14.0",
            "target": {"name": "protocol_codec", "kind": ["bench"], "crate_types": ["bin"]},
            "executable": str(executable or self.exe), "profile": {"opt_level": "3", "test": True},
            "features": ["default", "render-diff"], "fresh": True,
        }
        message.update(changes)
        return message

    def write_log(self, *messages):
        self.log.write_text("\n".join(message if isinstance(message, str) else json.dumps(message)
                                      for message in messages) + "\n")

    def run_mocked_bench(self, *, failure=None, emit_artifact=True):
        def run(command, **kwargs):
            if emit_artifact:
                kwargs["stdout"].write(json.dumps(self.artifact()) + "\n")
            kwargs["stdout"].write("original benchmark output\n")
            if failure:
                raise failure
            return subprocess.CompletedProcess(command, 0)
        with mock.patch.object(bench.subprocess, "run", side_effect=run) as invoked:
            bench.run_benchmark(self.command, source=self.root, env={}, target=self.target,
                                log_path=self.log, out=self.out, label="candidate-1", capture_protocol=True)
            self.assertEqual(invoked.call_count, 1, "capturing an artifact must not recompile")

    def test_structured_artifact_ignores_human_output_and_unrelated_targets(self):
        self.write_log("Compiling ezpn", "Benchmarking frame_decode/4kb", [],
                       {"reason": "compiler-message", "message": "diagnostic"},
                       self.artifact(executable=None, target={"name": "other", "kind": ["bench"]}),
                       self.artifact(executable=None, target={"name": "protocol_codec", "kind": ["lib"]}),
                       self.artifact(), self.artifact())
        executable, message = bench.protocol_compiler_artifact(self.log, self.target)
        self.assertEqual(executable, self.exe.resolve())
        self.assertTrue(message["fresh"], "a Cargo-reported cached artifact is still the selected executable")

    def test_null_executable_is_not_a_candidate(self):
        message = self.artifact()
        message["executable"] = None
        self.write_log(message, self.artifact())
        self.assertEqual(bench.protocol_compiler_artifact(self.log, self.target)[0], self.exe.resolve())

    def test_missing_message_never_falls_back_to_existing_cache_files(self):
        self.make_executable(self.target / "release/deps/protocol_codec-stale")
        self.write_log("Finished bench profile", {"reason": "build-finished", "success": True})
        with self.assertRaisesRegex(ValueError, "found 0"):
            bench.protocol_compiler_artifact(self.log, self.target)

    def test_distinct_matching_executables_are_ambiguous(self):
        second = self.make_executable(self.target / "release/deps/protocol_codec-other")
        self.write_log(self.artifact(), self.artifact(second))
        with self.assertRaisesRegex(ValueError, "found 2"):
            bench.protocol_compiler_artifact(self.log, self.target)

    def test_sibling_prefix_and_parent_traversal_are_outside_target(self):
        outside = self.make_executable(self.root / "dedicated target-other/protocol_codec")
        for path in [outside, self.target / "../dedicated target-other/protocol_codec"]:
            with self.subTest(path=path):
                self.write_log(self.artifact(path))
                with self.assertRaisesRegex(ValueError, "outside"):
                    bench.protocol_compiler_artifact(self.log, self.target)

    def test_symlink_escape_is_rejected(self):
        outside = self.make_executable(self.root / "outside/protocol_codec")
        link = self.target / "release/deps/protocol_codec-link"
        link.symlink_to(outside)
        self.write_log(self.artifact(link))
        with self.assertRaisesRegex(ValueError, "outside"):
            bench.protocol_compiler_artifact(self.log, self.target)

    def test_relative_or_invalid_executable_fields_are_rejected(self):
        for path in ["release/deps/protocol_codec", "", 42]:
            with self.subTest(path=path):
                message = self.artifact()
                message["executable"] = path
                self.write_log(message)
                with self.assertRaisesRegex(ValueError, "absolute"):
                    bench.protocol_compiler_artifact(self.log, self.target)

    def test_missing_directory_and_non_executable_paths_are_rejected(self):
        self.write_log(self.artifact(self.target / "missing"))
        with self.assertRaises(FileNotFoundError):
            bench.protocol_compiler_artifact(self.log, self.target)
        self.write_log(self.artifact(self.target))
        with self.assertRaises(ValueError):
            bench.protocol_compiler_artifact(self.log, self.target)
        self.exe.chmod(0o600)
        self.write_log(self.artifact())
        with self.assertRaises(ValueError):
            bench.protocol_compiler_artifact(self.log, self.target)

    def test_preserves_exact_bytes_hash_profile_and_each_candidate_run(self):
        first_bytes = self.exe.read_bytes()
        self.write_log(self.artifact())
        bench.preserve_protocol_executable(self.log, self.target, self.out, "candidate-1", self.command)
        self.assertEqual((self.out / "protocol-codec-candidate").read_bytes(), first_bytes)
        metadata = json.loads((self.out / "protocol-codec-candidate.json").read_text())
        self.assertEqual(metadata["sha256"], hashlib.sha256(first_bytes).hexdigest())
        self.assertEqual(metadata["source_executable"], str(self.exe.resolve()))
        self.assertEqual(metadata["profile"], {"opt_level": "3", "test": True})
        self.assertEqual(metadata["command"], self.command)
        self.assertEqual(metadata["cargo_exit_code"], 0)
        self.exe.write_bytes(b"\x7fELFrelinked second run")
        bench.preserve_protocol_executable(self.log, self.target, self.out, "candidate-2", self.command)
        self.assertEqual((self.out / "protocol-codec-candidate").read_bytes(), first_bytes)
        self.assertEqual((self.out / "protocol-codec-candidate-1").read_bytes(), first_bytes)
        self.assertEqual((self.out / "protocol-codec-candidate-2").read_bytes(), self.exe.read_bytes())

    def test_baseline_gets_its_own_binary_and_metadata(self):
        self.write_log(self.artifact())
        bench.preserve_protocol_executable(self.log, self.target, self.out, "baseline", self.command)
        self.assertEqual((self.out / "protocol-codec-baseline").read_bytes(), self.exe.read_bytes())
        metadata = json.loads((self.out / "protocol-codec-baseline.json").read_text())
        self.assertEqual(metadata["label"], "baseline")
        self.assertEqual(metadata["artifact"], "protocol-codec-baseline")

    def test_no_run_smoke_is_explicitly_unmeasured(self):
        self.write_log(self.artifact())
        command = self.command[:self.command.index("--")] + ["--no-run"]
        bench.preserve_protocol_executable(self.log, self.target, self.out, "smoke-unmeasured", command)
        metadata = json.loads((self.out / "protocol-codec-smoke-unmeasured.json").read_text())
        self.assertEqual(metadata["execution_kind"], "unmeasured-no-run")
        self.assertEqual(metadata["label"], "smoke-unmeasured")
        self.assertFalse((self.out / "protocol-codec-candidate").exists())

    def test_smoke_label_requires_cargo_no_run_not_a_benchmark_argument(self):
        self.write_log(self.artifact())
        with self.assertRaisesRegex(ValueError, "unexpected benchmark label"):
            bench.preserve_protocol_executable(self.log, self.target, self.out, "smoke-unmeasured", self.command + ["--no-run"])

    def test_successful_subprocess_captures_without_an_extra_build(self):
        self.run_mocked_bench()
        self.assertTrue((self.out / "protocol-codec-candidate").is_file())

    def test_original_benchmark_failure_is_preserved_with_executable(self):
        failure = subprocess.CalledProcessError(42, self.command)
        with self.assertRaises(subprocess.CalledProcessError) as raised:
            self.run_mocked_bench(failure=failure)
        self.assertIs(raised.exception, failure)
        metadata = json.loads((self.out / "protocol-codec-candidate-1.json").read_text())
        self.assertEqual(metadata["cargo_exit_code"], 42)
        self.assertIn("original benchmark output", self.log.read_text())

    def test_original_failure_is_not_hidden_by_missing_capture(self):
        failure = subprocess.CalledProcessError(7, self.command)
        with self.assertRaises(subprocess.CalledProcessError) as raised:
            self.run_mocked_bench(failure=failure, emit_artifact=False)
        self.assertIs(raised.exception, failure)
        self.assertTrue((self.out / "protocol-codec-candidate-1-capture-error.json").is_file())

    def test_error_report_failure_does_not_replace_benchmark_failure(self):
        failure = subprocess.CalledProcessError(9, self.command)
        with mock.patch.object(Path, "write_text", side_effect=PermissionError("report unavailable")):
            with self.assertRaises(subprocess.CalledProcessError) as raised:
                self.run_mocked_bench(failure=failure)
        self.assertIs(raised.exception, failure)

    def test_timeout_retains_artifact_and_remains_a_timeout(self):
        failure = subprocess.TimeoutExpired(self.command, 600)
        with self.assertRaises(subprocess.TimeoutExpired) as raised:
            self.run_mocked_bench(failure=failure)
        self.assertIs(raised.exception, failure)
        metadata = json.loads((self.out / "protocol-codec-candidate-1.json").read_text())
        self.assertTrue(metadata["timed_out"])
        self.assertIsNone(metadata["cargo_exit_code"])

    def test_success_without_reported_artifact_fails_closed(self):
        with self.assertRaisesRegex(ValueError, "found 0"):
            self.run_mocked_bench(emit_artifact=False)


if __name__ == "__main__":
    unittest.main()
