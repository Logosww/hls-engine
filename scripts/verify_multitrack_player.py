#!/usr/bin/env python3
"""Chrome + pinned Shaka: independently extract wvtt, parse and render actual output.
Run verify_multitrack.py first. Supply the compiled debug Shaka distribution.
This adapter is acceptance tooling, not a direct mixed-mdat playback claim.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import signal
import struct
import subprocess
import tempfile
import threading
from functools import partial
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from verify_multitrack import ROOT, OUT, boxes, probe


def box(kind, data):
    return struct.pack('>I', len(data)+8)+kind+data


def extract(path):
    data = path.read_bytes()
    metadata = probe(path)
    moov = next(body for kind, body in boxes(data) if kind == b'moov')
    extracted = []
    for stream in metadata['streams']:
        if stream['codec_tag_string'] != 'wvtt':
            continue
        track = int(stream['id'], 16)
        contents = b''
        for kind, body in boxes(moov):
            if kind == b'mvhd':
                contents += box(kind, body)
            if kind == b'trak':
                tkhd = next(b for k, b in boxes(body) if k == b'tkhd')
                offset = 20 if tkhd[0] == 1 else 12
                if int.from_bytes(tkhd[offset:offset+4], 'big') == track:
                    contents += box(kind, body)
        contents += box(b'mvex', box(b'trex', struct.pack('>6I', 0, track, 1, 0, 0, 0)))
        init = OUT/f'extracted-{path.stem}-{track}.init.mp4'
        init.write_bytes(box(b'ftyp', b'isom\0\0\0\0isomiso6mp41')+box(b'moov', contents))
        fragments = b''
        for seq, packet in enumerate(p for p in metadata['packets'] if p['stream_index'] == stream['index']):
            pos, size = int(packet['pos']), int(packet['size'])
            header = box(b'mfhd', struct.pack('>2I', 0, seq+1))
            traf = box(b'tfhd', struct.pack('>2I', 0x020000, track))+box(b'tfdt', struct.pack('>IQ', 0x01000000, int(packet['pts'])))
            def moof(offset):
                return box(b'moof', header+box(b'traf', traf+box(b'trun', struct.pack('>5I', 0x301, 1, offset, int(packet['duration']), size))))
            fragments += moof(len(moof(0))+8)+box(b'mdat', data[pos:pos+size])
        media = init.with_name(init.name.replace('.init.', '.media.'))
        media.write_bytes(fragments)
        extracted.append({'init': init.name, 'media': media.name, 'track': track, 'language': stream['tags']['language']})
    return extracted


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--shaka', type=Path, required=True)
    args = parser.parse_args()
    shaka = args.shaka.resolve()
    assert shaka.is_relative_to(ROOT), 'Shaka must be under the served repository root'
    cases = []
    for mode in ['fragmented', 'classic']:
        file = OUT/f'multitrack-{mode}.mp4'
        dash = OUT/('dash-'+mode)
        dash.mkdir(exist_ok=True)
        subprocess.run(['ffmpeg', '-v', 'error', '-y', '-i', str(file), '-map', '0:v', '-map', '0:a',
                        '-c', 'copy', '-f', 'dash', '-adaptation_sets', 'id=0,streams=0 id=1,streams=1 id=2,streams=2',
                        str(dash/'manifest.mpd')], check=True)
        cases.append({'file': file.name, 'tracks': extract(file), 'dash': 'dash-'+mode+'/manifest.mpd'})
    (OUT/'player-cases.json').write_text(json.dumps(cases))
    page = OUT/'player.html'
    page.write_text('''<!doctype html><meta charset="utf-8"><style>body{margin:0}#container{position:relative;width:640px;height:360px;background:#222;color:white}video{width:100%;height:100%}.shaka-text-container{position:absolute;inset:0}</style><div id="container"><video id="video" muted></video></div><script src="/'''+str(shaka.relative_to(ROOT))+'''"></script><script type="module" src="/tests/support/multitrack-player.mjs"></script>''')
    done = threading.Event()
    results = []
    class Handler(SimpleHTTPRequestHandler):
        def log_message(self, *_):
            pass
        def do_POST(self):
            length = int(self.headers.get('Content-Length', '0'))
            if self.path != '/result' or not 0 < length < 1048576:
                self.send_error(400)
                return
            results.append(json.loads(self.rfile.read(length)))
            self.send_response(204)
            self.end_headers()
            done.set()
    with ThreadingHTTPServer(('127.0.0.1', 0), partial(Handler, directory=str(ROOT))) as server:
        worker = threading.Thread(target=server.serve_forever, daemon=True)
        worker.start()
        try:
            with tempfile.TemporaryFile(mode='w+') as log:
                process = subprocess.Popen(['node', 'scripts/run_multitrack_browser.mjs', f'http://127.0.0.1:{server.server_port}/target/multitrack/player.html'], stdout=log, stderr=log, start_new_session=True)
                try:
                    completed = done.wait(90)
                    if completed:
                        # Let the runner close its browser before escalating cleanup.
                        try:
                            process.wait(timeout=5)
                        except subprocess.TimeoutExpired:
                            pass
                finally:
                    for sig in (signal.SIGTERM, signal.SIGKILL):
                        if process.poll() is not None:
                            break
                        try:
                            os.killpg(process.pid, sig)
                        except (ProcessLookupError, PermissionError):
                            if process.poll() is None:
                                process.send_signal(sig)
                        try:
                            process.wait(timeout=3)
                        except subprocess.TimeoutExpired:
                            pass
                log.seek(0)
                assert completed and process.returncode == 0, log.read()[-4000:]
                result = results[0]
                result['shakaSha256'] = hashlib.sha256(shaka.read_bytes()).hexdigest()
                result['adapter'] = 'extract original wvtt sample payload using independent ffprobe offsets; single text track init + fragments'
                (OUT/'player-evidence.json').write_text(json.dumps(result, ensure_ascii=False, indent=2)+'\n')
                assert result['status'] == 'PASS', result
                print(json.dumps(result, ensure_ascii=False))
        finally:
            server.shutdown()
            worker.join()


if __name__ == '__main__':
    main()
