#!/usr/bin/env python3
"""Compile the real SDK's native/WASM adapters against this local crate.

Cargo patches, the tracked timeline extension and build products live in
target/sdk-compat; the SDK checkout is read-only. Tests exercise the actual SDK
host and WASM Promise bridge. No package is published or SDK lock changed.
"""
import argparse
import hashlib
import json
import os
import re
import sdk_timeline_cases
import verify_sdk_browser
import shutil
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def run(*command, **kwargs):
    subprocess.run(command, check=True, **kwargs)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('sdk', type=Path)
    args = parser.parse_args()
    sdk = args.sdk.expanduser().resolve()
    output = ROOT / 'target/sdk-compat'
    copy = output / 'source'
    if copy.exists():
        shutil.rmtree(copy)
    adapters = Path('packages/adapters/src')
    for path in [adapters / 'node', adapters / 'browser/crates', adapters / 'rust', Path('test/fixtures')]:
        shutil.copytree(sdk / path, copy / path, dirs_exist_ok=True,
                        ignore=shutil.ignore_patterns('target', 'node_modules', '*.node'))
    version = re.search(r'^version = "([^"]+)"$', (ROOT / 'Cargo.toml').read_text(), re.M).group(1)
    for manifest in (copy / adapters).rglob('Cargo.toml'):
        manifest.write_text(re.sub(r'(hls-transmux\s*=\s*\{[^\n]*?version\s*=\s*")[^"]+',
                                   lambda m: m.group(1) + version, manifest.read_text()))
    patch = f'patch.crates-io.hls-transmux.path={json.dumps(str(ROOT))}'
    native = copy / adapters / 'node/Cargo.toml'
    browser = copy / adapters / 'browser/crates/hls-transmux-wasm/Cargo.toml'
    target = str(output / 'build')
    common = ['--offline', '--config', patch, '--target-dir', target]
    run('cargo', 'test', '--manifest-path', str(native), '-p', 'hls-core',
        '--test', 'prepared_upstream', '--test', 'resume_upstream', *common)
    run('cargo', 'check', '--manifest-path', str(native), '--workspace', *common)
    run('cargo', 'check', '--manifest-path', str(browser),
        '--target', 'wasm32-unknown-unknown', *common)
    # Install the versioned integration extension only into the disposable SDK copy.
    # Both runtimes keep the SDK's real SourceHost/provider/Promise implementation.
    (output / 'cases.json').write_text(json.dumps(sdk_timeline_cases.cases(), indent=2) + '\n')
    shared = copy / adapters / 'rust/keyed.rs'
    shared_text = shared.read_text()
    # Versioned v0.8 adapter extension, installed only in the disposable copy.
    shared_text = shared_text.replace('"method":k.method().as_str(),', '"method":k.method().as_str(),"kid":r.resource().kid().map(|kid|kid.iter().map(|b|format!("{b:02x}")).collect::<String>()),', 1)
    shared_text = shared_text.replace('let mut key = AvailableKey::aes128(secret);', '''let mut key = match r.reference().method().as_str() {
                        "SAMPLE-AES" => AvailableKey::sample_aes(secret),
                        "SAMPLE-AES-CTR" => AvailableKey::sample_aes_ctr(secret),
                        _ => AvailableKey::aes128(secret),
                    };
                    if let Some(kid) = r.resource().kid() { key = key.with_kid(kid); }''')
    shared.write_text(shared_text + '\n#[path = "timeline.rs"]\npub mod timeline;\n')
    shutil.copyfile(ROOT / 'tests/support/sdk/shared.rs', shared.with_name('timeline.rs'))
    browser_source = browser.parent / 'src/keyed.rs'
    browser_source.write_text(browser_source.read_text() + '\n' + (ROOT / 'tests/support/sdk/browser.rs').read_text())
    browser.write_text(browser.read_text().replace('default-features = false', 'default-features = false, features = ["serde"]', 1))
    native_test = native.parent / 'crates/core/tests/timeline_upstream.rs'
    shutil.copyfile(ROOT / 'tests/support/sdk/native.rs', native_test)
    run('cargo', 'test', '--manifest-path', str(native), '-p', 'hls-core', '--test', 'timeline_upstream',
        *common, env={**os.environ, 'HLS_TIMELINE_ROOT': str(ROOT)})
    run('cargo', 'build', '--manifest-path', str(browser), '--target', 'wasm32-unknown-unknown', *common)
    run('wasm-bindgen', str(output / 'build/wasm32-unknown-unknown/debug/hls_transmux_browser_wasm.wasm'),
        '--target', 'web', '--out-dir', str(output / 'pkg'))
    browser_evidence = verify_sdk_browser.verify()
    # Verify the resolved dependency rather than trusting a CLI patch was used.
    for manifest in [native, browser]:
        metadata = json.loads(subprocess.check_output([
            'cargo', 'metadata', '--offline', '--format-version', '1',
            '--manifest-path', str(manifest), '--config', patch,
        ]))
        upstream = next(p for p in metadata['packages'] if p['name'] == 'hls-transmux')
        assert Path(upstream['manifest_path']).resolve() == ROOT / 'Cargo.toml'
    digest = hashlib.sha256()
    for path in [ROOT / 'Cargo.toml', *sorted((ROOT / 'src').rglob('*.rs'))]:
        digest.update(str(path.relative_to(ROOT)).encode())
        digest.update(path.read_bytes())
    integrated = any('prepare_hls_timeline' in path.read_text()
                     for path in (copy / adapters).rglob('*.rs'))
    evidence = {
        'sdkRevision': subprocess.check_output(['git', '-C', str(sdk), 'rev-parse', 'HEAD'], text=True).strip(),
        'upstreamSourceSha256': digest.hexdigest(),
        'nativePreparedAndResumeTests': True,
        'nativeWorkspaceCompiled': True,
        'browserWasmCompiled': True,
        'timelineApiIntegratedInHarness': integrated,
        'timelineCases': browser_evidence['cases'],
        'sampleEncryptionAdapterExtension': True,
        'sampleCancellationCases': browser_evidence['cancelledSamples'],
        'lateSampleCompletions': browser_evidence['lateSampleCompletions'],
        'nativeBrowserTimelineEqual': browser_evidence['nativeBrowserTimelineEqual'],
        'sdkCheckoutModified': False,
        'scope': 'Real SDK shared host and browser Promise adapter with the tracked timeline integration extension; range, dual input, AES, TS SAMPLE-AES, fMP4 cenc/cbcs, KID Promise transport, reset, gaps and split outputs. SDK application rollout is independent.',
    }
    (output / 'evidence.json').write_text(json.dumps(evidence, indent=2) + '\n')
    print(json.dumps(evidence, indent=2))


if __name__ == '__main__':
    main()
