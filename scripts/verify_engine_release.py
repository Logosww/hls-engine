#!/usr/bin/env python3
"""Verify package identity, release tag and clean checkout before publication."""
import argparse
import hashlib
import os
from pathlib import Path
import re
import subprocess

ROOT = Path(__file__).resolve().parents[1]


def source_digest():
    """Fingerprint the checked-out source for generated CI recovery artifacts."""
    names = subprocess.check_output(
        ['git', 'ls-files', '--cached', '--others', '--exclude-standard', '-z'],
        cwd=ROOT,
    ).split(b'\0')
    files = {ROOT / os.fsdecode(name) for name in names if name}
    digest = hashlib.sha256()
    for path in sorted(path for path in files if path.is_file()):
        relative = path.relative_to(ROOT).as_posix().encode()
        content = path.read_bytes()
        digest.update(len(relative).to_bytes(8, 'big'))
        digest.update(relative)
        digest.update(len(content).to_bytes(8, 'big'))
        digest.update(content)
    return digest.hexdigest()


def require(condition, message):
    if not condition:
        raise SystemExit(message)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source-digest', action='store_true')
    args = parser.parse_args()
    if args.source_digest:
        print(source_digest())
        return
    manifest = (ROOT / 'Cargo.toml').read_text()
    section = re.search(r'(?ms)^\[package\]\s*\n(.*?)(?=^\[|\Z)', manifest)
    require(section, 'missing package metadata')
    package = dict(re.findall(r'^\s*(name|version)\s*=\s*"([^"\n]+)"\s*$',
                              section.group(1), re.M))
    require(package.get('name') == 'hls-engine', 'wrong publication package')
    version = package.get('version', '')
    require(re.fullmatch(r'\d+\.\d+\.\d+', version), 'a final release version is required')
    ref = os.environ.get('GITHUB_REF_NAME')
    if ref:
        require(ref in (version, 'v' + version), 'release tag does not match package version')
    require(not subprocess.check_output(['git', 'status', '--porcelain'], cwd=ROOT).strip(),
            'release checkout is dirty')
    print(f'hls-engine {version} release identity verified')


if __name__ == '__main__':
    main()
