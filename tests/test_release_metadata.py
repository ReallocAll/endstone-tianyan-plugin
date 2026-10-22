import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("release_metadata", Path(__file__).parents[1] / "tools/release_metadata.py")
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class ReleaseMetadataTest(unittest.TestCase):
    def test_prepared_release_and_ordinary_build(self):
        self.assertEqual(module.resolve("1.2.5", "## [1.2.5]", "push", "refs/heads/master", "chore: release v1.2.5"), ("1.2.5", True))
        self.assertEqual(module.resolve("1.2.5", "", "push", "refs/heads/master", "fix: ownership"), ("1.2.5", False))

    def test_rejects_mismatch_and_missing_notes(self):
        with self.assertRaises(ValueError):
            module.resolve("1.2.5", "## [1.2.5]", "push", "refs/tags/v1.2.4", "")
        with self.assertRaises(ValueError):
            module.resolve("1.2.5", "", "push", "refs/heads/master", "chore: release v1.2.5")
        with self.assertRaises(ValueError):
            module.resolve("1.2.5", "## [1.2.5]", "push", "refs/heads/master", "chore: release v1.2.5 extra")

    def test_manual_dry_run_does_not_publish(self):
        self.assertEqual(module.resolve("1.2.5", "## [1.2.5]", "workflow_dispatch", "refs/heads/master", "", "1.2.5"), ("1.2.5", False))


if __name__ == "__main__":
    unittest.main()
