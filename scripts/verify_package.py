#!/usr/bin/env python3
"""Dry-run publication and build/run examples using only the extracted crate."""
import argparse
import hashlib
import json
import re
import subprocess
import tarfile
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MAX_COMPRESSED_BYTES = 1024 * 1024
PUBLIC_EXAMPLES = {
    'continuous_demo', 'continuous_wasm', 'engine_demo', 'keyed_demo',
    'keyed_wasm', 'multitrack_demo', 'multitrack_wasm', 'prepared_demo',
    'transmux_demo', 'wasm_demo', 'wasm_session',
}
# Both include_bytes! and runtime file reads in the library's unit tests matter.
# wasm_session also embeds a small TS/video + fMP4/audio contract input.
PUBLIC_FIXTURES = {
    'crypto/README.md', 'crypto/vector.cbc', 'gcm/vector.gcm', 'h264_aac_fhd.ts',
    'media/fmp4_avc_nal2_multirun/init.fmp4',
    'media/fmp4_avc_nal2_multirun/seg0.m4s', 'media/ts_avc_regular/seg0.ts',
    'media/ts_avc_video_only/seg0.ts', 'media/ts_avc_video_only/seg1.ts',
    'media/ts_avc_video_only/seg2.ts', 'media/fmp4_aac_audio_only/init.fmp4',
    'media/fmp4_aac_audio_only/seg0.m4s', 'media/fmp4_aac_audio_only/seg1.m4s',
    'media/fmp4_aac_audio_only/seg2.m4s', 'media/fmp4_aac_audio_only/seg3.m4s',
    'sample_crypto/fmp4_avc_cenc/init.mp4',
    'sample_crypto/fmp4_avc_clear/init.mp4', 'sample_crypto/fmp4_avc_clear/seg1.m4s',
    'sample_crypto/fmp4_hevc_clear/init.mp4', 'sample_crypto/fmp4_hevc_clear/seg1.m4s',
}
REQUIRED = {
    'src/engine.rs', 'src/state_codec.rs', 'docs/engine.md',
    'docs/README.md', 'docs/support.md', 'docs/runtime-tests.md',
    'docs/multitrack-sessions.md', 'docs/continuous-sessions.md',
    'docs/timeline-sessions.md', 'docs/keyed-sessions.md', 'docs/keyed-wasm.md',
} | {f'examples/{name}.rs' for name in PUBLIC_EXAMPLES} | {
    f'tests/fixtures/{name}' for name in PUBLIC_FIXTURES
}


def inspect_archive(archive, name):
    """Reject size regressions and test corpora before building the package."""
    size = archive.stat().st_size
    assert size <= MAX_COMPRESSED_BYTES, (
        f'compressed package exceeds 1 MiB: {size} > {MAX_COMPRESSED_BYTES} bytes')
    with tarfile.open(archive) as tar:
        members = tar.getmembers()
        assert all(item.name.startswith(name + '/') for item in members)
        assert all(item.isfile() or item.isdir() for item in members)
        files = {item.name.removeprefix(name + '/'): item.size
                 for item in members if item.isfile()}
    entries = set(files)
    assert REQUIRED <= entries, f'missing package files: {sorted(REQUIRED - entries)}'
    unexpected = {path for path in entries if (
        path.startswith(('tests/', 'examples/')) and path not in REQUIRED
    ) or path.startswith(('scripts/', '.github/', 'target/', 'docs/planning/', 'docs/release-'))}
    assert not unexpected, f'repository-only package files: {sorted(unexpected)}'
    return {'package': name, 'compressedBytes': size,
            'compressedLimitBytes': MAX_COMPRESSED_BYTES,
            'uncompressedBytes': sum(files.values()), 'fileCount': len(files),
            'sha256': hashlib.sha256(archive.read_bytes()).hexdigest(),
            'files': files}


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
    name = f'hls-engine-{version}'
    archive = ROOT / 'target/package' / f'{name}.crate'
    evidence = inspect_archive(archive, name)
    print(json.dumps({key: value for key, value in evidence.items() if key != 'files'}), flush=True)
    with tempfile.TemporaryDirectory(prefix='hls-package-') as temporary:
        with tarfile.open(archive) as tar:
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
        run('cargo', 'build', '--offline', '--locked', '--features', 'serde,experimental-gcm',
            '--examples', '--target-dir', str(target), cwd=crate)
        for features in ([], ['--no-default-features'],
                         ['--features', 'serde,experimental-gcm'],
                         ['--no-default-features', '--features', 'serde,experimental-gcm']):
            run('cargo', 'test', '--offline', '--locked', *features,
                '--lib', '--target-dir', str(target), cwd=crate)
        run('cargo', 'test', '--offline', '--locked', '--features', 'serde,experimental-gcm',
            '--doc', '--target-dir', str(target), cwd=crate)
        run('cargo', 'run', '--offline', '--locked', '--example', 'engine_demo', '--target-dir', str(target), cwd=crate)
        run('cargo', 'build', '--offline', '--locked', '--no-default-features',
            '--target', 'wasm32-unknown-unknown', '--example', 'keyed_wasm',
            '--target-dir', str(target), cwd=crate)
        bindings = Path(temporary) / 'bindings'
        run('wasm-bindgen', str(target / 'wasm32-unknown-unknown/debug/examples/keyed_wasm.wasm'),
            '--target', 'web', '--out-dir', str(bindings))
        run('node', str(ROOT / 'scripts/test_keyed_wasm.mjs'),
            str(bindings / 'keyed_wasm.js'), str(ROOT), cwd=crate)
        run('cargo', 'build', '--offline', '--locked', '--no-default-features', '--features', 'serde',
            '--target', 'wasm32-unknown-unknown', '--example', 'continuous_wasm', '--target-dir', str(target), cwd=crate)
        run('wasm-bindgen', str(target / 'wasm32-unknown-unknown/debug/examples/continuous_wasm.wasm'),
            '--target', 'web', '--out-dir', str(bindings))
        run('node', str(ROOT / 'scripts/test_continuous_wasm.mjs'), str(bindings / 'continuous_wasm.js'), str(ROOT), cwd=crate)
        run('cargo', 'build', '--offline', '--locked', '--no-default-features', '--features', 'serde',
            '--target', 'wasm32-unknown-unknown', '--example', 'multitrack_wasm', '--target-dir', str(target), cwd=crate)
        run('wasm-bindgen', str(target / 'wasm32-unknown-unknown/debug/examples/multitrack_wasm.wasm'),
            '--target', 'web', '--out-dir', str(bindings))
        run('node', str(ROOT / 'scripts/test_multitrack_wasm.mjs'), str(bindings / 'multitrack_wasm.js'), str(ROOT), cwd=crate)
        # Host-supplied WASM buffers above come from repository corpora; only the
        # extracted Rust library/examples are compiled. This adapter embeds its
        # own contract inputs, so execute it without any repository fixture reads.
        run('cargo', 'build', '--offline', '--locked', '--no-default-features',
            '--target', 'wasm32-unknown-unknown', '--example', 'wasm_session',
            '--target-dir', str(target), cwd=crate)
        run('node', str(ROOT / 'scripts/test_wasm_session.mjs'),
            str(target / 'wasm32-unknown-unknown/debug/examples/wasm_session.wasm'), cwd=crate)
    evidence.update({'publishDryRun': True, 'extractedExamples': True,
                     'extractedUnitTestMatrix': True, 'extractedDoctests': True,
                     'wasmRuntimeExamples': 4})
    output = ROOT / 'target/runtime/package-evidence.json'
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(evidence, indent=2) + '\n')
    print(json.dumps({key: value for key, value in evidence.items() if key != 'files'}))


if __name__ == '__main__':
    main()
