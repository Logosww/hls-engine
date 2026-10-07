#!/usr/bin/env python3
"""CI regressions for release identity and source provenance."""
import os
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch
import verify_engine_release as release


class ReleaseGuard(unittest.TestCase):
    def test_new_source_is_included_before_staging(self):
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            subprocess.run(['git', 'init', '-q', name], check=True)
            source = root / 'src.rs'
            source.write_text('first')
            with patch.object(release, 'ROOT', root):
                first = release.source_digest()
                source.write_text('changed')
                self.assertNotEqual(first, release.source_digest())
                second = release.source_digest()
                subprocess.run(['git', 'add', 'src.rs'], cwd=root, check=True)
                self.assertEqual(second, release.source_digest())

    def test_tag_identity_and_final_version(self):
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            manifest = root / 'Cargo.toml'
            manifest.write_text('[package]\nname = "hls-engine"\nversion = "1.0.0"\n')
            with patch.object(release, 'ROOT', root), patch('sys.argv', ['verify_engine_release.py']), \
                    patch.dict(os.environ, {'GITHUB_REF_NAME': 'v1.0.0'}), \
                    patch.object(subprocess, 'check_output', return_value=b''):
                release.main()
                with patch.dict(os.environ, {'GITHUB_REF_NAME': 'v0.10.0'}):
                    with self.assertRaisesRegex(SystemExit, 'tag does not match'):
                        release.main()
                manifest.write_text('[package]\nname = "hls-engine"\nversion = "1.0.0-rc.1"\n')
                with self.assertRaisesRegex(SystemExit, 'final release version'):
                    release.main()

    def test_dirty_checkout_is_rejected(self):
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            (root / 'Cargo.toml').write_text('[package]\nname = "hls-engine"\nversion = "1.0.0"\n')
            with patch.object(release, 'ROOT', root), patch('sys.argv', ['verify_engine_release.py']), \
                    patch.dict(os.environ, {'GITHUB_REF_NAME': 'v1.0.0'}), \
                    patch.object(subprocess, 'check_output', return_value=b' M README.md\n'):
                with self.assertRaisesRegex(SystemExit, 'dirty'):
                    release.main()


if __name__ == '__main__':
    unittest.main()
