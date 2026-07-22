import unittest
from pathlib import Path
import tempfile

import prepare


class PrepareTests(unittest.TestCase):
    def test_excluded_test_ids_are_deduplicated_across_backends(self):
        records = [
            {"id": "generic/067", "backend": "memory", "disposition": "excluded"},
            {"id": "generic/067", "backend": "chunked_local", "disposition": "excluded"},
        ]

        self.assertEqual(
            prepare.rendered_excludes(records),
            "# Generated from exceptions.toml by runner/prepare.py. Do not edit.\n"
            "generic/067\n",
        )

    def test_reduced_generic_014_requires_exact_reviewed_shape(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "exceptions.toml"
            path.write_text(
                """schema = 1

[[exceptions]]
id = "generic/014"
backend = "object_s3"
disposition = "reduced"
reason = "Whole-object storage repeats full object rewrites."
tracking_issue = "ISSUES.md#f-014"
reduction = "truncfile-iterations"
full_iterations = 10000
reduced_iterations = 100
focused_coverage = "object sparse semantic regression"
""",
                encoding="utf-8",
            )

            records = prepare.load_exceptions(path)
            self.assertEqual(records[0]["reduced_iterations"], 100)

    def test_reduced_generic_011_requires_exact_reviewed_shape(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "exceptions.toml"
            path.write_text(
                """schema = 1

[[exceptions]]
id = "generic/011"
backend = "chunked_local"
disposition = "reduced"
reason = "The quick matrix retains process topology without nightly saturation."
tracking_issue = "README.md#result-policy"
reduction = "dirstress-files"
full_iterations = 1000
reduced_iterations = 20
focused_coverage = "nightly full dirstress process matrix"
""",
                encoding="utf-8",
            )

            records = prepare.load_exceptions(path)
            self.assertEqual(records[0]["reduced_iterations"], 20)

    def test_reduction_pair_is_exact(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "exceptions.toml"
            path.write_text(
                """schema = 1

[[exceptions]]
id = "generic/011"
backend = "chunked_local"
disposition = "reduced"
reason = "This reduction name belongs to a different exact test."
tracking_issue = "README.md#result-policy"
reduction = "truncfile-iterations"
full_iterations = 1000
reduced_iterations = 100
focused_coverage = "invalid pair"
""",
                encoding="utf-8",
            )

            with self.assertRaisesRegex(ValueError, "unsupported exact test/reduction pair"):
                prepare.load_exceptions(path)

    def test_reduced_generic_371_requires_exact_reviewed_shape(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "exceptions.toml"
            path.write_text(
                """schema = 1

[[exceptions]]
id = "generic/371"
backend = "chunked_local"
disposition = "reduced"
reason = "The quick matrix retains the concurrent race without endurance volume."
tracking_issue = "README.md#result-policy"
reduction = "parallel-enospc-iterations"
full_iterations = 100
reduced_iterations = 5
focused_coverage = "nightly full parallel ENOSPC race"
""",
                encoding="utf-8",
            )

            records = prepare.load_exceptions(path)
            self.assertEqual(records[0]["reduced_iterations"], 5)

    def test_reduced_generic_129_requires_exact_reviewed_shape(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "exceptions.toml"
            path.write_text(
                """schema = 1

[[exceptions]]
id = "generic/129"
backend = "chunked_local"
disposition = "reduced"
reason = "The quick matrix retains every looptest mode at a reviewed ratio."
tracking_issue = "README.md#result-policy"
reduction = "looptest-iterations"
full_iterations = 162000
reduced_iterations = 1620
focused_coverage = "nightly full looptest workload"
""",
                encoding="utf-8",
            )

            records = prepare.load_exceptions(path)
            self.assertEqual(records[0]["reduced_iterations"], 1620)

    def test_reduced_generic_404_requires_exact_reviewed_shape(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "exceptions.toml"
            path.write_text(
                """schema = 1

[[exceptions]]
id = "generic/404"
backend = "chunked_local"
disposition = "reduced"
reason = "The quick matrix retains patterned insert and full-file verification."
tracking_issue = "README.md#result-policy"
reduction = "insert-range-blocks"
full_iterations = 500
reduced_iterations = 20
focused_coverage = "nightly full insert-range workload"
""",
                encoding="utf-8",
            )

            records = prepare.load_exceptions(path)
            self.assertEqual(records[0]["reduced_iterations"], 20)

    def test_reduced_generic_471_requires_exact_reviewed_shape(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "exceptions.toml"
            path.write_text(
                """schema = 1

[[exceptions]]
id = "generic/471"
backend = "chunked_local"
disposition = "reduced"
reason = "The quick matrix retains the rewinddir contract without endurance volume."
tracking_issue = "README.md#result-policy"
reduction = "rewinddir-files"
full_iterations = 10000
reduced_iterations = 512
focused_coverage = "nightly full rewinddir workload"
""",
                encoding="utf-8",
            )

            records = prepare.load_exceptions(path)
            self.assertEqual(records[0]["reduced_iterations"], 512)

    def test_reduced_generic_488_requires_exact_reviewed_shape(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "exceptions.toml"
            path.write_text(
                """schema = 1

[[exceptions]]
id = "generic/488"
backend = "chunked_local"
disposition = "reduced"
reason = "The quick matrix retains open-unlink descriptor lifetime semantics."
tracking_issue = "README.md#result-policy"
reduction = "open-unlink-files"
full_iterations = 10000
reduced_iterations = 512
focused_coverage = "nightly full open-unlink workload"
""",
                encoding="utf-8",
            )

            records = prepare.load_exceptions(path)
            self.assertEqual(records[0]["reduced_iterations"], 512)

    def test_reduced_generic_676_requires_exact_reviewed_shape(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "exceptions.toml"
            path.write_text(
                """schema = 1

[[exceptions]]
id = "generic/676"
backend = "chunked_local"
disposition = "reduced"
reason = "The quick matrix retains seekdir and getdents correctness coverage."
tracking_issue = "README.md#result-policy"
reduction = "seekdir-files"
full_iterations = 4000
reduced_iterations = 256
focused_coverage = "nightly full seekdir workload"
""",
                encoding="utf-8",
            )

            records = prepare.load_exceptions(path)
            self.assertEqual(records[0]["reduced_iterations"], 256)

    def test_reduction_fields_fail_closed_on_other_dispositions(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "exceptions.toml"
            path.write_text(
                """schema = 1

[[exceptions]]
id = "generic/014"
backend = "object_s3"
disposition = "excluded"
reason = "This must not accept a hidden reduction field."
reduced_iterations = 1000
""",
                encoding="utf-8",
            )

            with self.assertRaisesRegex(ValueError, "reduction fields require"):
                prepare.load_exceptions(path)

if __name__ == "__main__":
    unittest.main()
