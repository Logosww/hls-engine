#!/usr/bin/env python3
"""Produce actual v0.10.0 schema-v1 artifacts, then resume using hls-engine legacy."""
import io
import json
from pathlib import Path
import shutil
import subprocess
import tarfile

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / 'target/legacy-checkpoint'
BASELINE = 'f0960b3'


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    baseline = OUT / 'baseline'
    baseline.mkdir(exist_ok=True)
    archive = subprocess.check_output(['git', 'archive', BASELINE, 'Cargo.toml', 'Cargo.lock', 'README.md', 'LICENSE', 'src', 'docs'], cwd=ROOT)
    with tarfile.open(fileobj=io.BytesIO(archive)) as tar:
        for entry in tar:
            path = baseline / entry.name
            assert path.resolve().is_relative_to(baseline.resolve())
            if entry.isdir():
                path.mkdir(parents=True, exist_ok=True)
            else:
                assert entry.isfile()
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(tar.extractfile(entry).read())
    harness = OUT / 'harness'
    (harness / 'src').mkdir(parents=True, exist_ok=True)
    manifest = '''[package]
name = "legacy-checkpoint-verifier"
version = "0.0.0"
edition = "2024"
publish = false
[dependencies]
old = { package = "hls-transmux", path = "../baseline", features = ["serde"] }
current = { package = "hls-engine", path = "../../..", features = ["serde"] }
tokio = { version = "1", features = ["rt", "macros"] }
serde_json = "1"
'''
    (harness / 'Cargo.toml').write_text(manifest)
    shutil.copyfile(ROOT / 'tests/support/legacy_checkpoint.rs', harness / 'src/main.rs')
    media = OUT / 'media'
    media.mkdir(exist_ok=True)
    for path in (ROOT / 'tests/fixtures/media/ts_avc_regular').iterdir():
        shutil.copyfile(path, media / path.name)
    # Keep old produced artifacts and resumed artifacts in separate per-run directories.
    import tempfile
    with tempfile.TemporaryDirectory(prefix='run-', dir=OUT) as temporary:
        for path in media.iterdir():
            shutil.copyfile(path, Path(temporary) / path.name)
        result = subprocess.check_output(['cargo', 'run', '--offline', '--manifest-path', str(harness / 'Cargo.toml'),
            '--target-dir', str(OUT / 'build'), '--', temporary], cwd=ROOT, text=True)
        evidence = json.loads(result)
        evidence['baselineCommit'] = subprocess.check_output(['git', 'rev-parse', BASELINE], cwd=ROOT, text=True).strip()
        for path in Path(temporary).glob('*.checkpoint.json'):
            shutil.copyfile(path, OUT / path.name)
    (OUT / 'evidence.json').write_text(json.dumps(evidence, indent=2) + '\n')
    print(json.dumps(evidence))


if __name__ == '__main__':
    main()
