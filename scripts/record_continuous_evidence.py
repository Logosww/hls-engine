#!/usr/bin/env python3
"""Assemble v0.9 evidence from completed local verification runs; never executes gates."""
import hashlib
import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

def read(path):
    return json.loads((ROOT / path).read_text())

def main():
    matrix = read('target/continuous-matrix.json')
    for row in matrix:
        text = Path(row.pop('log')).read_text()
        row['logSha256'] = hashlib.sha256(text.encode()).hexdigest()
        row['testGroups'] = [dict(zip(['passed', 'failed', 'ignored'], map(int, group)))
                             for group in re.findall(r'test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored', text)]
    native = read('target/runtime/native-continuous-evidence.json')
    runtime = {}
    for name in ['node', 'browser']:
        source = read(f'target/runtime/{name}-evidence.json')
        runtime[name] = {'runtime': source['runtime'], 'cases': len(source['report']['continuous']),
                         'bridge': source['bridge']['continuous'], 'profile': source['profile']['continuous']}
        runtime[name]['equal'] = source.get('nativeWasmEqual', source.get('nativeWasmBrowserEqual', False))
    evidence = {
        'version': '0.9.0', 'baseline': '8d6c54f', 'date': '2026-10-06',
        'platform': 'macOS arm64', 'published': False, 'sdkProductModified': False,
        'continuousIntegrationTests': 23,
        'commands': matrix,
        'independentDecode': read('target/continuous-output/evidence.json'),
        'runtime': runtime, 'nativeProfile': native,
        'sdk': read('target/sdk-compat/evidence.json'),
        'package': read('target/continuous-package.json') if (ROOT/'target/continuous-package.json').exists() else {'status':'pending'},
        'additionalChecks': read('target/continuous-additional.json') if (ROOT/'target/continuous-additional.json').exists() else [],
        'limits': ['Controlled 8/64/256 epoch repetition; not a multi-hour production benchmark.',
                   'Caller snapshots, collector bytes, classic sample indexes and callback storage are separate costs.',
                   'No new persistent checkpoint; no registry publication or SDK product rollout.'],
    }
    for name in ['sample_crypto', 'crypto', 'media']:
        path = ROOT/'tests/fixtures'/name/'manifest.json'
        evidence[name+'ManifestSha256'] = hashlib.sha256(path.read_bytes()).hexdigest()
    (ROOT/'docs/release-0.9.0-evidence.json').write_text(json.dumps(evidence,indent=2)+'\n')
    print('Recorded completed gate results; no unrun gate was marked successful.')

if __name__ == '__main__':
    main()
