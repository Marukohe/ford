"""Checks for release boundaries that could publish the wrong version or assets."""

import hashlib
from pathlib import Path
import tempfile
import unittest

from release import TARGETS, archive_name, changelog_notes, check_version, checksums


class ReleaseTests(unittest.TestCase):
    def test_tag_must_match_package_version(self):
        self.assertEqual(check_version("v0.1.0", "0.1.0"), {"version": "0.1.0", "prerelease": False})
        for tag in ["0.1.0", "v0.1.1", "v0.1.0/other", "v0.1.0\n"]:
            with self.assertRaises(ValueError):
                check_version(tag, "0.1.0")

    def test_prerelease_is_not_inferred_from_build_metadata(self):
        self.assertTrue(check_version("v0.2.0-rc.1", "0.2.0-rc.1")["prerelease"])
        self.assertFalse(check_version("v0.2.0+build-one", "0.2.0+build-one")["prerelease"])

    def test_curated_notes_stop_before_the_previous_version(self):
        text = "# Changelog\n\n## [Unreleased]\n\nLater.\n\n## [0.2.0]\n\n### Fixed\n\n- New fix.\n\n"
        text += "## [0.1.0]\n\n- Old feature.\n"
        self.assertEqual(changelog_notes(text, "0.2.0"), "### Fixed\n\n- New fix.\n")
        self.assertIsNone(changelog_notes(text, "0.3.0"))
        with self.assertRaises(ValueError):
            changelog_notes("## [0.1.0]\n\n## [0.0.1]\nOld.\n", "0.1.0")

    def test_checksums_require_the_complete_exact_asset_set(self):
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp)
            for target in TARGETS:
                (directory / archive_name("v0.1.0", target)).write_bytes(target.encode())
            manifest = checksums("v0.1.0", directory)
            for line in manifest.read_text().splitlines():
                digest, filename = line.split("  ")
                self.assertEqual(digest, hashlib.sha256((directory / filename).read_bytes()).hexdigest())
            self.assertEqual(len(manifest.read_text().splitlines()), len(TARGETS))
            (directory / archive_name("v0.1.0", TARGETS[0])).unlink()
            with self.assertRaises(ValueError):
                checksums("v0.1.0", directory)
            (directory / "unexpected.tar.gz").write_bytes(b"wrong version")
            with self.assertRaises(ValueError):
                checksums("v0.1.0", directory)

    def test_local_checksum_mode_and_unsupported_targets(self):
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp)
            target = TARGETS[0]
            (directory / archive_name("v0.1.0", target)).write_bytes(b"native archive")
            self.assertTrue(checksums("v0.1.0", directory, [target]).is_file())
        with self.assertRaises(ValueError):
            archive_name("v0.1.0", "made-up-target")


if __name__ == "__main__":
    unittest.main()
