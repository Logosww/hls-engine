#!/usr/bin/env python3
"""Dry-run publication and build/run examples using only the extracted crate."""
import argparse
import json
import re
import subprocess
import tarfile
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def run(*args, cwd=ROOT):
    subprocess.run(args, cwd=cwd, check=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--allow-dirty', action='store_true')
    args = parser.parse_args()
    version = re.search(r'^version = "([^"]+)"$', (ROOT / 'Cargo.toml').read_text(), re.M).group(1)
    command = ['cargo', 'publish', '--dry-run', '--locked']
    if args.allow_dirty:
        command.append('--allow-dirty')
    run(*command)
    # publish --dry-run may delete its temporary archive and leave an older
    # .crate on disk. Produce the archive explicitly before inspecting it.
    package = ['cargo', 'package', '--offline', '--locked', '--no-verify']
    if args.allow_dirty:
        package.append('--allow-dirty')
    run(*package)
    name = f'hls-transmux-{version}'
    archive = ROOT / 'target/package' / f'{name}.crate'
    required = {
        'docs/README.md', 'docs/typed-playlists.md', 'docs/key-sessions.md',
        'docs/aes-resources.md', 'docs/keyed-sessions.md', 'docs/keyed-contracts.md',
        'docs/keyed-wasm.md', 'docs/prepared-sessions.md', 'docs/benchmarks.md',
        'docs/release-0.6.0.md', 'docs/release-0.6.0-evidence.json',
        'docs/runtime-tests.md', 'docs/writer-streaming-api.md', 'examples/keyed_export.rs', 'examples/keyed_demo.rs', 'examples/keyed_wasm.rs',
        'scripts/test_keyed_wasm.mjs',
    }
    with tempfile.TemporaryDirectory(prefix='hls-package-') as temporary:
        with tarfile.open(archive) as tar:
            entries = {item.name.removeprefix(name + '/') for item in tar.getmembers()}
            assert required <= entries, f'missing package files: {required - entries}'
            assert not any(path.startswith(('tests/runtime/', 'docs/planning/', 'target/')) for path in entries)
            for item in tar.getmembers():
                path = Path(temporary) / item.name
                assert path.resolve().is_relative_to(Path(temporary).resolve())
                assert item.isfile() or item.isdir()
                if item.isdir():
                    path.mkdir(parents=True, exist_ok=True)
                else:
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_bytes(tar.extractfile(item).read())
        crate = Path(temporary) / name
        for guide in [crate / 'README.md', *(crate / 'docs').glob('*.md')]:
            for link in re.findall(r'\]\(([^)]+)\)', guide.read_text()):
                if '://' not in link and not link.startswith('#'):
                    assert (guide.parent / link.split('#')[0]).exists(), (guide, link)
        # A separate output directory avoids relying on repository build products.
        target = ROOT / 'target/package-examples'
        run('cargo', 'build', '--offline', '--locked', '--example', 'keyed_demo', '--example', 'keyed_export',
            '--target-dir', str(target), cwd=crate)
        run('cargo', 'build', '--offline', '--locked', '--no-default-features',
            '--target', 'wasm32-unknown-unknown', '--example', 'keyed_wasm',
            '--target-dir', str(target), cwd=crate)
        bindings = Path(temporary) / 'bindings'
        run('wasm-bindgen', str(target / 'wasm32-unknown-unknown/debug/examples/keyed_wasm.wasm'),
            '--target', 'web', '--out-dir', str(bindings))
        run('node', str(crate / 'scripts/test_keyed_wasm.mjs'),
            str(bindings / 'keyed_wasm.js'), str(crate), cwd=crate)
    print(json.dumps({'package': name, 'requiredFiles': len(required),
                      'publishDryRun': True, 'extractedExamples': True}))


if __name__ == '__main__':
    main()
