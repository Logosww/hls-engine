#!/usr/bin/env python3
"""macOS prefetch-only RSS benchmark. Local generated HTTP data; no media decoding."""
import json
import pathlib
import re
import subprocess
ROOT = pathlib.Path(__file__).resolve().parents[1]
subprocess.run(['cargo', 'build', '--offline', '--release', '--example', 'prefetch_benchmark'], cwd=ROOT, check=True)
for size in (1024*1024, 8*1024*1024):
    for concurrency in (1, 4):
        result = subprocess.run(['/usr/bin/time', '-l', str(ROOT/'target/release/examples/prefetch_benchmark'), str(size), str(concurrency)], cwd=ROOT, text=True, capture_output=True, check=True)
        report = json.loads(result.stdout)
        report['peak_rss_bytes'] = int(re.search(r'(\d+)\s+maximum resident set size', result.stderr)[1])
        print(json.dumps(report), flush=True)
