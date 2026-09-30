#!/usr/bin/env python3
"""Generate a continuous B-frame HLS input and independently verify Native finalize.
Run: python3 scripts/verify_media.py (requires ffmpeg, ffprobe, cargo).
"""
import json
import pathlib
import struct
import subprocess
import tempfile

ROOT = pathlib.Path(__file__).resolve().parents[1]


def run(*args):
    return subprocess.check_output(args, cwd=ROOT, stderr=subprocess.PIPE).decode()


def packets(path):
    doc = json.loads(run('ffprobe', '-v', 'error', '-show_streams', '-show_packets',
                         '-show_data_hash', 'sha256', '-of', 'json', str(path)))
    streams = {s['index']: s for s in doc['streams']}
    grouped = {}
    for p in doc['packets']:
        s = streams[p['stream_index']]
        key = s['codec_type']
        grouped.setdefault(key, []).append((p, s))
    return grouped


def verify(source, output):
    a, b = packets(source), packets(output)
    assert a.keys() == b.keys()
    for kind in a:
        assert len(a[kind]) == len(b[kind]), (kind, len(a[kind]), len(b[kind]))
        for (before, sa), (after, sb) in zip(a[kind], b[kind]):
            assert sa['time_base'] == sb['time_base'], (kind, sa['time_base'], sb['time_base'])
            for field in ('dts', 'pts', 'duration', 'size', 'data_hash'):
                assert before.get(field) == after.get(field), (kind, field, before.get(field), after.get(field))
        print(f'{kind}: {len(a[kind])} samples, matching DTS/PTS/duration/payload')
    run('ffmpeg', '-v', 'error', '-xerror', '-i', str(output), '-f', 'null', '-')
    run('ffmpeg', '-v', 'error', '-xerror', '-ss', '3', '-i', str(output), '-t', '1', '-f', 'null', '-')
    with output.open('rb') as f:
        types = []
        while header := f.read(8):
            size, kind = struct.unpack('>I4s', header)
            if size == 1:
                size = struct.unpack('>Q', f.read(8))[0]
                head = 16
            else:
                head = 8
            types.append(kind)
            f.seek(size - head, 1)
    assert types == [b'ftyp', b'moov', b'mdat'], types


def main():
    run('cargo', 'build', '--offline', '--example', 'transmux_demo')
    with tempfile.TemporaryDirectory(prefix='hls-transmux-media-') as directory:
        folder = pathlib.Path(directory)
        for mode in ('ts', 'fmp4'):
            playlist = folder / f'input_{mode}.m3u8'
            run('ffmpeg', '-v', 'error', '-f', 'lavfi', '-i', 'testsrc2=size=320x180:rate=30',
                '-f', 'lavfi', '-i', 'sine=frequency=440:sample_rate=48000', '-t', '6',
                '-c:v', 'libx264', '-pix_fmt', 'yuv420p', '-g', '60', '-bf', '2',
                '-sc_threshold', '0', '-c:a', 'aac', '-b:a', '96k',
                '-f', 'hls', '-hls_time', '2', '-hls_list_size', '0',
                '-hls_segment_type', 'mpegts' if mode == 'ts' else 'fmp4', str(playlist))
            fragment = folder / f'fragment_{mode}.mp4'
            classic = folder / f'classic_{mode}.mp4'
            demo = str(ROOT / 'target/debug/examples/transmux_demo')
            run(demo, str(playlist), str(fragment), '--fragmented')
            run(demo, str(playlist), str(classic), '--streaming')
            print(f'Input: {mode}')
            verify(fragment, classic)
            print('Native faststart output decoded and seeked successfully.')



if __name__ == '__main__':
    main()
