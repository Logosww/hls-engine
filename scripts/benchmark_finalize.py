#!/usr/bin/env python3
"""Measure file-backed Native mux RSS in fresh release-mode processes.
Default: six cases (~2 GiB peak logical temp storage). --large adds >4 GiB.
Input payload is sparse zeros; this measures file I/O and index costs, not decoding.
"""
import json
import os
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
command = ['cargo', 'test', '--offline', '--release', '--lib', '--no-default-features',
           '--no-run', '--message-format=json']
build = subprocess.check_output(command, cwd=ROOT, text=True)
executable = next(json.loads(line)['executable'] for line in build.splitlines()
                  if json.loads(line).get('executable'))
cases = [(128, size) for size in (256*1024, 2*1024*1024, 8*1024*1024)]
cases += [(count, 4096) for count in (1024, 16384, 131072)]
if '--large' in sys.argv:
    cases.append((5120, 1024*1024))
results = []
for count, size in cases:
    env = dict(os.environ, HLS_BENCH_SAMPLES=str(count), HLS_BENCH_SAMPLE_BYTES=str(size))
    timing = ['-l'] if sys.platform == 'darwin' else ['-v']
    result = subprocess.run(['/usr/bin/time', *timing, executable,
                             'isobmff::file_scan::tests::finalize_benchmark',
                             '--exact', '--ignored', '--nocapture'],
                            env=env, cwd=ROOT, capture_output=True, text=True, check=True)
    data = json.loads(next(line[6:] for line in result.stdout.splitlines() if line.startswith('BENCH ')))
    if sys.platform == 'darwin':
        rss = int(re.search(r'(\d+)\s+maximum resident set size', result.stderr).group(1))
    else:
        rss = int(re.search(r'Maximum resident set size \(kbytes\): (\d+)', result.stderr).group(1))*1024
    data['peak_rss_bytes'] = rss
    results.append(data)
    print(json.dumps(data), flush=True)

# Noise allowance still catches accidental complete-payload buffering.
payload_rss = [r['peak_rss_bytes'] for r in results[:3]]
assert max(payload_rss) - min(payload_rss) < 16*1024*1024, 'RSS scaled with payload'
assert results[5]['peak_rss_bytes'] < results[3]['peak_rss_bytes'] + (131072-1024)*512, 'index overhead exceeded budget'
