#!/usr/bin/env python3
"""Issue #2: container duration and packet clocks agree for offset external audio.

Generates independent clear/AES inputs locally; requires FFmpeg, FFprobe, OpenSSL.
"""
import argparse
from contextlib import nullcontext
from functools import partial
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
import json
import os
import shutil
import subprocess
import tempfile
import threading
from fractions import Fraction
from pathlib import Path
from verify_media import ROOT, packets, run
from verify_multi_input import verify


def verify_browser(root, cases):
    """Exercise the browser's MP4 demuxer via video.duration, using local files."""
    chrome = (os.environ.get('HLS_TEST_CHROME') or shutil.which('google-chrome') or
              shutil.which('chromium') or '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome')
    done = threading.Event()
    reports = []

    class Handler(SimpleHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_POST(self):
            reports.append(json.loads(self.rfile.read(int(self.headers['Content-Length']))))
            self.send_response(204)
            self.end_headers()
            done.set()

    (root / 'browser.html').write_text('''<!doctype html><meta charset="utf-8"><script>
    (async () => {
      const results = [];
      try {
        for (const test of CASES) {
          const video = document.createElement('video');
          video.preload = 'metadata';
          video.src = test.path;
          await new Promise((resolve, reject) => {
            const timeout = setTimeout(() => reject(new Error('metadata timeout')), 10000);
            video.onloadedmetadata = () => { clearTimeout(timeout); resolve(); };
            video.onerror = () => { clearTimeout(timeout); reject(new Error('media error')); };
          });
          if (Math.abs(video.duration - test.duration) > 1 / 48000)
            throw new Error(`${test.path}: ${video.duration} != ${test.duration}`);
          results.push({path: test.path, duration: video.duration});
          video.removeAttribute('src'); video.load();
        }
        await fetch('/result', {method: 'POST', body: JSON.stringify({status: 'PASS', results})});
      } catch (error) {
        await fetch('/result', {method: 'POST', body: JSON.stringify({status: 'FAIL', error: String(error)})});
      }
    })();</script>'''.replace('CASES', json.dumps(cases)))
    with ThreadingHTTPServer(('127.0.0.1', 0), partial(Handler, directory=str(root))) as server:
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            with tempfile.TemporaryDirectory(prefix='hls-offset-chrome-') as profile, tempfile.TemporaryFile() as log:
                process = subprocess.Popen([chrome, '--headless', '--disable-gpu', '--no-first-run',
                    '--no-default-browser-check', '--disable-background-networking', '--disable-extensions',
                    '--user-data-dir=' + profile, f'http://127.0.0.1:{server.server_port}/browser.html'],
                    stdout=log, stderr=log)
                try:
                    assert done.wait(120), 'Browser metadata test timed out'
                    assert len(reports) == 1 and reports[0]['status'] == 'PASS', reports
                    assert len(reports[0]['results']) == len(cases)
                    (root / 'browser-evidence.json').write_text(json.dumps(reports[0], indent=2) + '\n')
                    print(f'Chrome video.duration: {len(cases)} outputs passed.', flush=True)
                finally:
                    process.terminate()
                    try:
                        process.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait()
        finally:
            server.shutdown()
            thread.join()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output-dir', type=Path, help='Retain generated fixtures and outputs.')
    parser.add_argument('--browser', action='store_true', help='Also check real Chrome video.duration.')
    options = parser.parse_args()
    run('cargo', 'build', '--offline', '--example', 'keyed_decode_export')
    exporter = str(ROOT / 'target/debug/examples/keyed_decode_export')
    outputs = 0
    with (nullcontext(options.output_dir) if options.output_dir else
          tempfile.TemporaryDirectory(prefix='hls-offset-')) as tmp:
        root = Path(tmp)
        cases = []
        for primary_format in ('ts', 'fmp4'):
            audio_format = 'fmp4' if primary_format == 'ts' else 'ts'
            clear = root / f'clear-{primary_format}'
            for role, container in [('primary', primary_format), ('audio', audio_format)]:
                target = clear / role
                target.mkdir(parents=True, exist_ok=True)
                args = ['ffmpeg', '-y', '-v', 'error']
                if role == 'primary':
                    args += ['-f', 'lavfi', '-i', 'testsrc2=size=160x90:rate=10']
                args += ['-f', 'lavfi', '-i', 'sine=frequency=880:sample_rate=48000', '-t', '2']
                if role == 'primary':
                    args += ['-c:v', 'libx264', '-preset', 'ultrafast', '-g', '10',
                             '-sc_threshold', '0', '-pix_fmt', 'yuv420p']
                args += ['-c:a', 'aac', '-b:a', '64k', '-f', 'hls', '-hls_time',
                         '1' if role == 'primary' else '0.6', '-hls_list_size', '0']
                if container == 'fmp4':
                    args += ['-hls_segment_type', 'fmp4', '-hls_fmp4_init_filename', 'init.mp4']
                args += ['-hls_segment_filename', str(target / ('segment-%02d.' +
                         ('ts' if container == 'ts' else 'm4s'))), str(target / 'media.m3u8')]
                run(*args)
            encrypted = root / f'aes-{primary_format}'
            for role in ('primary', 'audio'):
                (encrypted / role).mkdir(parents=True, exist_ok=True)
                for path in (clear / role).iterdir():
                    out = encrypted / role / path.name
                    if path.suffix == '.m3u8':
                        out.write_text(path.read_text().replace('#EXTM3U\n',
                            '#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI="key",IV=0x0\n'))
                    else:
                        run('openssl', 'enc', '-aes-128-cbc', '-K', '11' * 16,
                            '-iv', '00' * 16, '-in', str(path), '-out', str(out))
            video, audio = (clear / role / 'media.m3u8' for role in ('primary', 'audio'))
            references = {'video': packets(video)['video'], 'audio': packets(audio)['audio']}
            origin = min(Fraction(p['dts']) * Fraction(s['time_base'])
                         for rows in references.values() for p, s in rows)
            end = max((int(p['pts']) + int(p['duration'])) * Fraction(s['time_base']) - origin
                      for rows in references.values() for p, s in rows)
            # FFmpeg rounds an input edit offset to nearest while our parser
            # truncates it. The common origin can therefore differ by one input
            # tick, including for the other track with a finer timescale.
            origin_tolerance = max(Fraction(s['time_base'])
                                   for rows in references.values() for _, s in rows)
            baseline = None
            for source in (clear, encrypted):
                folder = root / f'output-{source.name}'
                run(exporter, str(source / 'primary'), str(folder), str(source / 'audio'))
                for api in ('bytes', 'file', 'stream', 'stream-indexed'):
                    output = folder / f'{api}.mp4'
                    verify(video, audio, output, primary_format, audio_format, origin_tolerance)
                    actual = packets(output)
                    packet_end = max((int(p['pts']) + int(p['duration'])) * Fraction(s['time_base'])
                                     for rows in actual.values() for p, s in rows)
                    if baseline is None:
                        baseline = actual
                    for kind, rows in actual.items():
                        for (p, s), (q, t) in zip(rows, baseline[kind]):
                            for field in ('dts', 'pts', 'duration'):
                                assert abs(int(p[field]) * Fraction(s['time_base']) -
                                           int(q[field]) * Fraction(t['time_base'])) <= Fraction(s['time_base'])
                    metadata = json.loads(run('ffprobe', '-v', 'error', '-show_format',
                                              '-of', 'json', str(output)))
                    duration = Fraction(metadata['format']['duration'])
                    assert abs(packet_end - end) <= origin_tolerance + Fraction(1, 48000)
                    assert abs(duration - packet_end) <= Fraction(1, 48000), (
                        source.name, api, 'container duration', float(duration), 'packet end', float(packet_end))
                    outputs += 1
                    cases.append({'path': output.relative_to(root).as_posix(), 'duration': float(packet_end)})
                    print(f'{source.name}/{api}: duration={float(duration):.6f}, packets and decodes match', flush=True)
        if options.browser:
            verify_browser(root, cases)
    print(f'Verified {outputs} offset external-audio outputs.')


if __name__ == '__main__':
    main()
