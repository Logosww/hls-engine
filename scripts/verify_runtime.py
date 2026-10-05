#!/usr/bin/env python3
"""Run production contract regressions in native Rust, Node/WASM and optionally Chrome."""
import argparse
from functools import partial
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import tempfile
import threading

ROOT = Path(__file__).resolve().parents[1]
EVIDENCE = ROOT / 'target/runtime'


def run(*args):
    subprocess.run(args, cwd=ROOT, check=True)


def verify_browser():
    chrome = os.environ.get('HLS_TEST_CHROME') or shutil.which('google-chrome') or shutil.which('chromium') or '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome'
    completed = threading.Event()
    reports = []

    class Handler(SimpleHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_POST(self):
            length = int(self.headers.get('Content-Length', '0'))
            if self.path != '/result' or not 0 < length <= 131072:
                self.send_error(400)
                return
            reports.append(json.loads(self.rfile.read(length)))
            self.send_response(204)
            self.end_headers()
            completed.set()

    with ThreadingHTTPServer(('127.0.0.1', 0), partial(Handler, directory=str(ROOT))) as server:
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            with tempfile.TemporaryDirectory(prefix='hls-runtime-chrome-') as profile, tempfile.TemporaryFile(mode='w+') as log:
                command = [chrome, '--headless', '--disable-gpu', '--no-first-run', '--no-default-browser-check',
                           '--disable-background-networking', '--disable-component-update', '--disable-extensions',
                           '--user-data-dir=' + profile, '--remote-debugging-port=0',
                           f'http://127.0.0.1:{server.server_port}/tests/runtime/browser.html']
                process = subprocess.Popen(command, stdout=log, stderr=log, start_new_session=True)
                try:
                    done = completed.wait(120)
                finally:
                    # Reap the isolated profile's helpers after the page completion handshake.
                    for sig in (signal.SIGTERM, signal.SIGKILL):
                        try:
                            os.killpg(process.pid, sig)
                        except (ProcessLookupError, PermissionError):
                            # A reaped leader or protected helper can outlive the
                            # process group permission. Still reap our own child.
                            if process.poll() is None:
                                process.send_signal(sig)
                        try:
                            process.wait(timeout=5)
                        except subprocess.TimeoutExpired:
                            pass
                log.seek(0)
                assert done, 'No browser report: ' + log.read()[-4000:]
                assert len(reports) == 1, reports
                report = reports[0]
                assert report['status'] == 'PASS', report
                node = json.loads((EVIDENCE / 'node-evidence.json').read_text())
                assert report['report'] == node['report'], 'browser/native/WASM mismatch'
                report['nativeWasmBrowserEqual'] = True
                (EVIDENCE / 'browser-evidence.json').write_text(json.dumps(report, indent=2) + '\n')
                print(json.dumps({'status': report['status'], 'runtime': report['runtime'],
                                  'nativeWasmBrowserEqual': True, 'bridge': report['bridge']}))
        finally:
            server.shutdown()
            thread.join()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--browser', action='store_true', help='Also run real headless Chrome (macOS/Linux).')
    args = parser.parse_args()
    run('cargo', 'build', '--locked', '--manifest-path', 'tests/runtime/Cargo.toml',
        '--target-dir', 'target', '--target', 'wasm32-unknown-unknown', '--lib')
    run('wasm-bindgen', 'target/wasm32-unknown-unknown/debug/hls_transmux_runtime_tests.wasm',
        '--target', 'web', '--out-dir', 'target/runtime/pkg')
    run('node', 'tests/runtime/test_node.mjs')
    if args.browser:
        verify_browser()


if __name__ == '__main__':
    main()
