#!/usr/bin/env python3
"""Regressions for the compressed-size and publication-content boundary."""
import io
import tarfile
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import verify_package as package


class PackageGuard(unittest.TestCase):
    def archive(self, root, extra=(), missing=()):
        archive = root / 'hls-engine-1.0.0.crate'
        with tarfile.open(archive, 'w:gz') as tar:
            for path in sorted((package.REQUIRED | set(extra)) - set(missing)):
                data = b'fixture'
                item = tarfile.TarInfo('hls-engine-1.0.0/' + path)
                item.size = len(data)
                tar.addfile(item, io.BytesIO(data))
        return archive

    def test_size_budget_includes_boundary_and_rejects_one_byte_over(self):
        with tempfile.TemporaryDirectory() as temporary:
            archive = self.archive(Path(temporary))
            size = archive.stat().st_size
            with patch.object(package, 'MAX_COMPRESSED_BYTES', size):
                evidence = package.inspect_archive(archive, 'hls-engine-1.0.0')
                self.assertEqual(evidence['compressedBytes'], size)
                self.assertEqual(evidence['fileCount'], len(package.REQUIRED))
            with patch.object(package, 'MAX_COMPRESSED_BYTES', size - 1):
                with self.assertRaisesRegex(AssertionError, 'exceeds 1 MiB'):
                    package.inspect_archive(archive, 'hls-engine-1.0.0')

    def test_repository_only_content_is_rejected_even_below_size_budget(self):
        for path in ('tests/engine_recovery.rs', 'tests/support/sample_crypto.rs',
                     'tests/fixtures/crypto/ts_avc_regular/bundle.bin',
                     'examples/engine_budget.rs', 'scripts/verify_media.py',
                     'docs/release-0.10.0-evidence.json'):
            with self.subTest(path=path), tempfile.TemporaryDirectory() as temporary:
                archive = self.archive(Path(temporary), extra=(path,))
                with self.assertRaisesRegex(AssertionError, 'repository-only'):
                    package.inspect_archive(archive, 'hls-engine-1.0.0')

    def test_missing_runtime_read_fixture_is_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            archive = self.archive(Path(temporary), missing=(
                'tests/fixtures/sample_crypto/fmp4_hevc_clear/seg1.m4s',))
            with self.assertRaisesRegex(AssertionError, 'missing package files'):
                package.inspect_archive(archive, 'hls-engine-1.0.0')


if __name__ == '__main__':
    unittest.main()
