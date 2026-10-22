import importlib.util
from pathlib import Path
import unittest
import os
import runpy
import tempfile
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("release_metadata", Path(__file__).parents[1] / "tools/release_metadata.py")
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class ReleaseMetadataTest(unittest.TestCase):
    def test_prepared_release_and_ordinary_build(self):
        self.assertEqual(module.resolve("1.2.5", "## [1.2.5]\nRelease notes\n", "push", "refs/heads/master", "chore: release v1.2.5"), ("1.2.5", True))
        self.assertEqual(module.resolve("1.2.5", "", "push", "refs/heads/master", "fix: ownership"), ("1.2.5", False))

    def test_rejects_mismatch_and_missing_notes(self):
        with self.assertRaises(ValueError):
            module.resolve("1.2.5", "## [1.2.5]\nRelease notes\n", "push", "refs/tags/v1.2.4", "")
        with self.assertRaises(ValueError):
            module.resolve("1.2.5", "", "push", "refs/heads/master", "chore: release v1.2.5")
        with self.assertRaises(ValueError):
            module.resolve("1.2.5", "## [1.2.5]\nRelease notes\n", "push", "refs/heads/master", "chore: release v1.2.5 extra")

    def test_manual_dry_run_does_not_publish(self):
        self.assertEqual(module.resolve("1.2.5", "## [1.2.5]\nRelease notes\n", "workflow_dispatch", "refs/heads/master", "", "1.2.5"), ("1.2.5", False))

    def test_main_emits_build_identity(self):
        script = Path(__file__).resolve().parents[1] / "tools/release_metadata.py"
        cwd = Path.cwd()
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            (root / "CMakeLists.txt").write_text('set(BASE_VERSION "1.2.5")\n')
            (root / "CHANGELOG.md").write_text("## [1.2.5]\nRelease notes\n")
            try:
                os.chdir(root)
                for event, subject, expected in [
                    ("push", "fix: ownership", "0"),
                    ("push", "chore: release v1.2.5", "1"),
                    ("workflow_dispatch", "fix: ownership", "1"),
                ]:
                    output = root / "output"
                    output.write_text("")
                    with patch.dict(os.environ, {"GITHUB_EVENT_NAME": event, "GITHUB_REF": "refs/heads/master", "GITHUB_OUTPUT": str(output), "INPUT_DRY_RUN": "true", "INPUT_VERSION": ""}), patch("subprocess.check_output", return_value=subject):
                        runpy.run_path(str(script), run_name="__main__")
                    self.assertIn(f"release_build={expected}\n", output.read_text())
            finally:
                os.chdir(cwd)

    def test_manual_dry_run_rejects_empty_notes(self):
        with self.assertRaises(ValueError):
            module.resolve("1.2.5", "## [1.2.5]\n\n## [1.2.4]\nOld notes", "workflow_dispatch", "refs/heads/master", "")


if __name__ == "__main__":
    unittest.main()
