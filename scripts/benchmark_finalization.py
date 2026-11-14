#!/usr/bin/env python3
"""Compare Native Copy and insufficient-reservation InPlace finalization.

Manual benchmark: writes dense synthetic fMP4 media, not sparse files or reflinks.
Default payloads are 256 MiB, 2 GiB and 8 GiB, three repeats. These are I/O/index
benchmarks, not valid AAC decode workloads. Correctness/player coverage lives in
verify_media.py. Requires ample free disk space (up to two payloads plus overhead).
"""
import argparse
import json
import os
from pathlib import Path
import platform
import statistics
import subprocess

ROOT = Path(__file__).resolve().parents[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--sizes-mib', default='256,2048,8192')
    parser.add_argument('--repeats', type=int, default=3)
    parser.add_argument('--output', type=Path, default=ROOT / 'target/finalization-benchmark.json')
    args = parser.parse_args()
    sizes = [int(value) for value in args.sizes_mib.split(',')]
    if not sizes or any(n < 64 or n > 32768 for n in sizes) or args.repeats < 1:
        parser.error('sizes must be 64..32768 MiB and repeats must be positive')
    env = dict(os.environ, HLS_FINALIZE_BENCH_MIB=','.join(map(str, sizes)),
               HLS_FINALIZE_BENCH_REPEATS=str(args.repeats))
    command = ['cargo', 'test', '--offline', '--release', '--lib',
               'native_finalization_benchmark', '--', '--ignored', '--nocapture']
    rows = []
    with subprocess.Popen(command, cwd=ROOT, env=env, stdout=subprocess.PIPE,
                          stderr=subprocess.STDOUT, text=True) as process:
        for line in process.stdout:
            print(line, end='', flush=True)
            if line.startswith('BENCH '):
                rows.append(json.loads(line[6:]))
        if process.wait():
            raise SystemExit(process.returncode)
    expected = sum((3 if n >= 8192 else 2) * args.repeats for n in sizes)
    if len(rows) != expected:
        raise RuntimeError(f'expected {expected} measurements, got {len(rows)}')
    summary = []
    for size in sizes:
        for mode in dict.fromkeys(row['mode'] for row in rows if row['payload_mib'] == size):
            group = [row for row in rows if row['payload_mib'] == size and row['mode'] == mode]
            summary.append({'payload_mib': size, 'mode': mode, 'runs': len(group), **{
                field: {'median': statistics.median(row[field] for row in group),
                        'min': min(row[field] for row in group),
                        'max': max(row[field] for row in group)}
                for field in ('scan_seconds', 'finalize_seconds', 'finalize_sync_seconds',
                              'prepare_seconds', 'pipeline_seconds', 'pipeline_sync_seconds')
            }})
    evidence = {'platform': platform.platform(), 'cpu_count': os.cpu_count(),
                'rustc': subprocess.check_output(['rustc', '--version'], text=True).strip(),
                'profile': 'release', 'sample_bytes': 16384, 'samples_per_fragment': 256,
                'cache_policy': 'new dense input per run; source sync_all before timing; OS cache not purged',
                'order_policy': 'rotate modes per repetition',
                'timing': 'finalize includes scan/index/output flush; sync is measured separately; pipeline adds synthetic generation plus input sync, not HLS network/download/decryption',
                'rows': rows, 'summary': summary}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(evidence, indent=2) + '\n')
    print(f'Evidence: {args.output}')


if __name__ == '__main__':
    main()
