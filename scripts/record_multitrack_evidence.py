#!/usr/bin/env python3
"""Record v0.10 release acceptance only after every positive/negative gate is executed."""
import hashlib
import json
from pathlib import Path
import re
import subprocess

ROOT = Path(__file__).resolve().parents[1]


def read(path):
    return json.loads((ROOT/path).read_text())


def main():
    digest = hashlib.sha256()
    for path in [ROOT/'Cargo.toml', *sorted((ROOT/'src').rglob('*.rs'))]:
        digest.update(str(path.relative_to(ROOT)).encode())
        digest.update(path.read_bytes())
    verification = hashlib.sha256()
    paths = [ROOT/'Cargo.lock', ROOT/'.github/workflows/ci.yml']
    for folder in ['scripts', 'tests', 'examples']:
        paths.extend(p for p in (ROOT/folder).rglob('*') if p.is_file()
                     and p.suffix in {'.rs', '.py', '.mjs', '.swift', '.html'} and 'target' not in p.parts)
    for path in sorted(paths):
        verification.update(str(path.relative_to(ROOT)).encode())
        verification.update(path.read_bytes())
    tests = {}
    for profile in ['all-features', 'no-default', 'serde', 'default', 'no-default-serde']:
        log = (ROOT/f'target/multitrack-{profile}.log').read_text()
        summaries = re.findall(r'test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored', log)
        assert summaries and 'error: test failed' not in log and 'could not compile' not in log, profile
        assert 'all doctests ran' in log, 'unfinished tests: '+profile
        tests[profile] = {'passed': sum(int(s[0]) for s in summaries), 'ignored': sum(int(s[2]) for s in summaries)}
    for name in ['clippy', 'runtime-clippy', 'wasm-clippy']:
        log = (ROOT/f'target/multitrack-{name}.log').read_text()
        assert 'Finished' in log and 'error:' not in log, name
    node = read('target/runtime/node-evidence.json')
    browser = read('target/runtime/browser-evidence.json')
    assert node['nativeWasmEqual'] and browser['nativeWasmBrowserEqual']
    assert len(node['report']['multitrack']) == 194
    assert node['report']['multitrack'] == browser['report']['multitrack']
    reports = read('target/multitrack/reports.json')
    assert [(r['name'], r['hash']) for r in reports] == [(r['name'], r['hash']) for r in node['report']['multitrack']]
    sdk = read('target/sdk-compat/evidence.json')
    assert sdk['nativeBrowserMultitrackEqual'] and sdk['multitrackCases'] == 5 and sdk['multitrackCancellationCases'] == 3
    assert sdk['upstreamSourceSha256'] == digest.hexdigest(), 'SDK evidence predates production source'
    media = read('target/multitrack/evidence.json')
    assert len(media['cases']) == 18
    assert media['matrixOutputs'] == 160 and media['splitOutputs'] == 16
    assert len(media['matrix']) == 40 and len(media['splits']) == 8
    player = read('target/multitrack/player-evidence.json')
    assert player['status'] == 'PASS'
    assert all(len(t['settings']) == 5 and t['settings'][0]['percentageLinePositionEqual']
               for r in player['results'] for t in r['tracks'])
    assert all(not r['fullMultitrack'] and 'timeout' not in r.get('error', '') for r in player['directMse'])
    assert len(player['adapterGeometry']) == 3 and all(abs(r['actual']-r['expected']) < .005 for r in player['adapterGeometry'])
    assert all(len(row['audioSwitches']) == 2 for row in player['results'])
    subprocess.run(['cargo', 'fmt', '--all', '--', '--check'], cwd=ROOT, check=True)
    package_log = (ROOT/'target/multitrack-package.log').read_text()
    package = json.loads(package_log.splitlines()[-1])
    assert package['publishDryRun'] and package['extractedExamples']
    assert 'Verified 148 outputs' in (ROOT/'target/multitrack-timeline-regression.log').read_text()
    assert len(re.findall('→ .*: passed', (ROOT/'target/multitrack-multi-input-regression.log').read_text())) == 48
    webkit = read('target/multitrack/webkit-evidence.json')
    assert webkit['status'] == 'PASS' and len(webkit['results']) == 2
    assert all(not r['fullMultitrack'] and r.get('appended') and r['textTracks'] == 0 for r in webkit['directMse'])
    assert all(len(r['audio']) == 2 and len(r['subtitles']) == 2 for r in webkit['results'])
    assert all(len(t['rendered']) == 6 and t['rendered'][4]['cues'][0]['position'] == 80
               for r in webkit['results'] for t in r['subtitles'])
    avfoundation = read('target/multitrack/avfoundation-evidence.json')
    for row in avfoundation['results']:
        assert row['audioSelection'] and row['subtitlesSelection'] and row['playable']
        assert len(row['decodedSubtitles']) == 2
        for track in row['decodedSubtitles']:
            assert {f'Settings {i}' for i in range(5)} <= {c['text'] for c in track['cues']}
    iina = read('target/multitrack/iina-evidence.json')
    assert iina['status'] == 'OBSERVED' and len(iina['results']) == 2
    assert all(r['subtitleTracks'] == 0 and len(r['audioSwitches']) == 2 for r in iina['results'])
    vlc = read('target/multitrack/vlc-evidence.json')
    assert vlc['status'] == 'OBSERVED' and len(vlc['results']) == 2
    assert all(len(r['subtitles']) == 2 and len(r['audioSwitches']) == 2 and len(r['rendered']) == 12 for r in vlc['results'])
    for result in [iina, vlc]:
        assert all(s['requested'] == s['selected'] for r in result['results'] for s in r['audioSwitches'])
    assert media['playerLimits'] and all(not r['defaultDecodeEqual'] and r['sampleDecodeIgnoringEditsEqual'] for r in media['playerLimits'])
    assert all(not r['defaultDecodeEqual'] for r in media['ffmpegSelfRemux'])
    assert any(not r['ffmpegDefaultTimelineEqual'] for r in media['matrix'])
    artifacts = ['target/runtime/node-evidence.json', 'target/runtime/browser-evidence.json',
                 'target/sdk-compat/evidence.json', 'target/multitrack/evidence.json',
                 'target/multitrack/player-evidence.json', 'target/multitrack/avfoundation-evidence.json',
                 'target/multitrack-budget.json', 'target/multitrack/iina-evidence.json',
                 'target/multitrack/vlc-evidence.json', 'target/multitrack/webkit-evidence.json']
    artifacts.extend(str(Path(r['screenshot']['path']).relative_to(ROOT)) for r in player['adapterGeometry'])
    for row in player['results']:
        for track in row['tracks']:
            shots = [track['overlapScreenshot'], *[s['screenshot'] for s in track['settings']]]
            artifacts.extend(str(Path(s['path']).relative_to(ROOT)) for s in shots)
    artifacts.extend('target/multitrack/'+s['screenshot'] for r in webkit['results'] for t in r['subtitles'] for s in t['rendered'])
    artifacts.extend(s['screenshot'] for r in vlc['results'] for s in r['rendered'])
    evidence = {
        'version': '0.10.0', 'baseline': 'fe97915', 'date': '2026-10-07',
        'status': 'acceptance-complete', 'published': False, 'releaseReady': True,
        'sourceSha256': digest.hexdigest(),
        'verificationSourceSha256': verification.hexdigest(),
        'regressions': {'timelineOutputs': 148, 'dualInputOutputs': 48},
        'verificationCommands': [
            'cargo test --all-features', 'cargo test', 'cargo test --features serde',
            'cargo test --no-default-features', 'cargo test --no-default-features --features serde',
            'python3 scripts/verify_multitrack.py', 'python3 scripts/verify_runtime.py --browser',
            'python3 scripts/verify_sdk.py /path/to/hls-downloader',
            'python3 scripts/verify_timeline.py --ffmpeg', 'python3 scripts/verify_multi_input.py --ffmpeg',
            'python3 scripts/verify_multitrack_player.py --shaka target/multitrack/shaka-player-5.2.12.js',
            'python3 scripts/verify_webkit.py',
            'scripts/probe_avfoundation.swift (compiled with swiftc; both layouts)',
            'scripts/probe_vlc.py (isolated VLC 3.0.24; both layouts)',
            'scripts/probe_iina.py (isolated IINA 1.5.0; both layouts)',
            'python3 scripts/verify_package.py --allow-dirty',
        ],
        'tests': tests, 'fmt': True, 'clippy': ['native-all-features', 'runtime-all-targets', 'wasm-example'],
        'multitrackIntegrationTests': 18, 'sharedTypedFailureChecks': 4,
        'nativeWasmChromeEqual': True, 'totalMultitrackOutputs': 194, 'outputs': media['cases'],
        'combinationMatrix': media['matrix'], 'splitMatrix': media['splits'],
        'packedProfiles': ['clear', 'aes128', 'sample_aes', 'aes128_rotation', 'sample_aes_rotation'],
        'nativeAllocations': read('target/multitrack-budget.json'),
        'nodeProfile': node['profile']['multitrack'], 'chromeProfile': browser['profile']['multitrack'],
        'sdk': sdk, 'package': package, 'shaka': player,
        'avfoundation': read('target/multitrack/avfoundation-evidence.json'),
        'ffmpegPlayerLimits': media['playerLimits'],
        'ffmpegSelfRemux': media['ffmpegSelfRemux'],
        'iina': iina, 'vlc': vlc,
        'webkit': read('target/multitrack/webkit-evidence.json'),
        'openGates': [],
        'closedGates': [
            {'gate': 'Shaka percentage line placement', 'outcome': 'fixed',
             'evidence': 'pinned shipped display adapter; all four file/language midpoint geometry checks pass'},
            {'gate': 'WebVTT settings fidelity', 'outcome': 'fixed',
             'evidence': 'reject invalid explicit auto tokens and numeric/duplicate syntax; valid settings retained by Shaka and native WebKit'},
            {'gate': 'frozen native player targets', 'outcome': 'executed',
             'evidence': 'AVFoundation timed decoding plus WebKit native snapshots; VLC actual screenshots and IINA 1.5.0 engine tests'},
            {'gate': 'direct browser blob/MSE', 'outcome': 'verified-unsupported',
             'enforcement': 'DirectBrowser -> BrowserTrackSelection; no implicit format conversion'},
            {'gate': 'VLC full wvtt settings', 'outcome': 'verified-unsupported',
             'enforcement': 'Vlc + subtitles -> SubtitleSettings',
             'visualReview': 'vlc-multitrack-classic-4-settings-1.png renders the vertical:rl cue horizontally near the bottom'},
            {'gate': 'IINA wvtt decoding', 'outcome': 'verified-unsupported',
             'enforcement': 'Iina + subtitles -> WvttDecoder; official engine loads, both layouts have zero subtitle tracks'},
            {'gate': 'FFmpeg classic default playback', 'outcome': 'verified-unsupported',
             'enforcement': 'Ffmpeg/Iina classic -> ClassicEditTimeline; independent fMP4 A/V path retained; default playback mismatch not counted as pass'},
        ],
        'artifacts': [{'path': p, 'sha256': hashlib.sha256((ROOT/p).read_bytes()).hexdigest()} for p in artifacts],
    }
    (ROOT/'docs/release-0.10.0-evidence.json').write_text(json.dumps(evidence, indent=2, ensure_ascii=False)+'\n')
    print(json.dumps({'status': evidence['status'], 'sourceSha256': evidence['sourceSha256'], 'outputs': 194, 'tests': tests}))


if __name__ == '__main__':
    main()
