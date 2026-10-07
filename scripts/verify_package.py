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
        'docs/multitrack-sessions.md', 'docs/release-0.10.0.md', 'docs/release-0.10.0-evidence.json',
        'examples/multitrack_demo.rs', 'examples/multitrack_wasm.rs', 'examples/multitrack_budget.rs',
        'tests/multitrack_session.rs', 'tests/support/multitrack_runtime.rs', 'tests/support/multitrack_profile.rs',
        'scripts/shaka_wvtt_adapter.mjs', 'scripts/run_multitrack_browser.mjs', 'scripts/multitrack_mse.mjs', 'scripts/verify_webkit.py', 'scripts/probe_vlc.py', 'scripts/record_multitrack_evidence.py',
        'scripts/verify_multitrack.py', 'scripts/verify_multitrack_matrix.py', 'scripts/probe_webkit.swift', 'scripts/probe_iina.py', 'scripts/verify_multitrack_player.py', 'scripts/test_multitrack_wasm.mjs',
        'tests/support/multitrack-player.mjs', 'scripts/probe_avfoundation.swift',
        'tests/fixtures/packed_aac/manifest.json',

        'docs/continuous-sessions.md', 'docs/release-0.9.0.md', 'docs/release-0.9.0-evidence.json',
        'examples/continuous_demo.rs', 'examples/continuous_wasm.rs', 'examples/continuous_budget.rs',
        'scripts/verify_continuous.py', 'scripts/test_continuous_wasm.mjs',
        'tests/continuous_session.rs', 'tests/support/continuous_runtime.rs', 'tests/support/continuous_profile.rs',
        'docs/README.md', 'docs/typed-playlists.md', 'docs/key-sessions.md',
        'docs/aes-resources.md', 'docs/keyed-sessions.md', 'docs/keyed-contracts.md',
        'docs/keyed-wasm.md', 'docs/prepared-sessions.md', 'docs/benchmarks.md',
        'docs/release-0.6.0.md', 'docs/release-0.6.0-evidence.json',
        'docs/release-0.6.1.md', 'examples/keyed_decode_export.rs',
        'scripts/verify_keyed_decode.py',
        'docs/release-0.6.2.md', 'scripts/verify_fragmented_timeline.py',
        'docs/runtime-tests.md', 'docs/writer-streaming-api.md', 'examples/keyed_export.rs', 'examples/keyed_demo.rs', 'examples/keyed_wasm.rs',
        'scripts/test_keyed_wasm.mjs',
        'docs/timeline-sessions.md', 'examples/timeline_export.rs', 'scripts/verify_timeline.py',
        'examples/timeline_budget.rs', 'tests/support/timeline_budget.rs',
        'tests/support/timeline_lifecycle.rs',
        'tests/support/timeline_combinations.rs', 'src/raw_sample/protection.rs',
        'scripts/verify_sdk.py', 'scripts/sdk_timeline_cases.py', 'scripts/verify_sdk_browser.py',
        'tests/support/sdk/shared.rs', 'tests/support/sdk/native.rs', 'tests/support/sdk/browser.rs', 'tests/support/sdk/browser.mjs',
        'src/transmux/session/timeline/catalog.rs', 'tests/support/allocation.rs', 'tests/support/timeline_profile.rs',
        'docs/release-0.7.0.md', 'docs/release-0.7.0-evidence.json',
        'docs/release-0.8.0.md', 'docs/release-0.8.0-evidence.json',
        'docs/sample-encryption.md', 'src/crypto/sample.rs', 'src/isobmff/protection.rs',
        'scripts/verify_sample_crypto.py', 'scripts/sample_fixture_layout.py',
        'tests/sample_crypto.rs', 'tests/support/sample_crypto.rs',
        'tests/support/sample_profile.rs', 'examples/sample_budget.rs',
        'tests/fixtures/sample_crypto/manifest.json',
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
        run('cargo', 'build', '--offline', '--locked', '--example', 'keyed_demo', '--example', 'keyed_export', '--example', 'timeline_export', '--example', 'timeline_budget',
            '--target-dir', str(target), cwd=crate)
        run('cargo', 'test', '--offline', '--locked', '--features', 'serde',
            '--test', 'timeline_session', '--test', 'sample_crypto', '--test', 'continuous_session', '--test', 'multitrack_session', '--target-dir', str(target), cwd=crate)
        run('cargo', 'build', '--offline', '--locked', '--no-default-features',
            '--target', 'wasm32-unknown-unknown', '--example', 'keyed_wasm',
            '--target-dir', str(target), cwd=crate)
        bindings = Path(temporary) / 'bindings'
        run('wasm-bindgen', str(target / 'wasm32-unknown-unknown/debug/examples/keyed_wasm.wasm'),
            '--target', 'web', '--out-dir', str(bindings))
        run('node', str(crate / 'scripts/test_keyed_wasm.mjs'),
            str(bindings / 'keyed_wasm.js'), str(crate), cwd=crate)
        run('cargo', 'build', '--offline', '--locked', '--example', 'continuous_demo', '--example', 'multitrack_demo', '--target-dir', str(target), cwd=crate)
        run('cargo', 'build', '--offline', '--locked', '--no-default-features', '--features', 'serde',
            '--target', 'wasm32-unknown-unknown', '--example', 'continuous_wasm', '--target-dir', str(target), cwd=crate)
        run('wasm-bindgen', str(target / 'wasm32-unknown-unknown/debug/examples/continuous_wasm.wasm'),
            '--target', 'web', '--out-dir', str(bindings))
        run('node', str(crate / 'scripts/test_continuous_wasm.mjs'), str(bindings / 'continuous_wasm.js'), str(crate), cwd=crate)
        run('cargo', 'build', '--offline', '--locked', '--no-default-features', '--features', 'serde',
            '--target', 'wasm32-unknown-unknown', '--example', 'multitrack_wasm', '--target-dir', str(target), cwd=crate)
        run('wasm-bindgen', str(target / 'wasm32-unknown-unknown/debug/examples/multitrack_wasm.wasm'),
            '--target', 'web', '--out-dir', str(bindings))
        run('node', str(crate / 'scripts/test_multitrack_wasm.mjs'), str(bindings / 'multitrack_wasm.js'), str(crate), cwd=crate)
    print(json.dumps({'package': name, 'requiredFiles': len(required),
                      'publishDryRun': True, 'extractedExamples': True}))


if __name__ == '__main__':
    main()
