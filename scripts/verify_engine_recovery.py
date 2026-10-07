#!/usr/bin/env python3
"""Replay every output in the retained 194-output corpus at every checkpoint.

Runs local native files only. Never tags, publishes, or marks a release gate ready.
"""
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import os
import time
from verify_engine_release import source_digest

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / 'target/engine-recovery-matrix'


def main():
    if OUT.exists():
        shutil.rmtree(OUT)
    OUT.mkdir(parents=True)
    before = source_digest()
    started = time.monotonic()
    command = ['cargo', 'run', '--offline', '--locked', '--release', '--features',
               'serde,experimental-gcm', '--example', 'multitrack_verify']
    with (OUT / 'run.log').open('w') as log:
        subprocess.run(command, cwd=ROOT, env={**os.environ, 'HLS_ENGINE_RECOVERY_MATRIX': str(OUT)},
                       stdout=log, stderr=subprocess.STDOUT, check=True)
    reports = json.loads((OUT / 'run.log').read_text().splitlines()[-1])
    records = [json.loads(p.read_text()) for p in sorted(OUT.glob('case-*/evidence.json'))]
    assert len(reports) == 194, len(reports)
    assert sum(row['outputs'] for row in records) == len(reports)
    assert all(row['allRecoveredEqual'] and row['checkpointWindows'] > 1 for row in records)
    assert before == source_digest(), 'source changed while recovery verification ran'
    evidence = {'sourceSha256': before, 'mediaOutputs': len(reports), 'sessions': len(records),
                'checkpointWindows': sum(row['checkpointWindows'] for row in records),
                'allRecoveredEqual': True, 'seconds': round(time.monotonic() - started, 3),
                'logSha256': hashlib.sha256((OUT / 'run.log').read_bytes()).hexdigest(), 'cases': records}
    (OUT / 'evidence.json').write_text(json.dumps(evidence, indent=2) + '\n')
    print(json.dumps({k: v for k, v in evidence.items() if k != 'cases'}))


if __name__ == '__main__':
    main()
