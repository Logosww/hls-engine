#!/usr/bin/env python3
"""Independent packet/payload/timeline checks of selected dual inputs; local only."""
import argparse
import hashlib
import json
import pathlib
import tempfile
from fractions import Fraction
from verify_media import ROOT, packets, normalized_payload, run, decode_and_seek, frame_hashes

CORPUS = ROOT / 'tests/fixtures/media'
EXTRA = ROOT / 'tests/fixtures/multi_input'

def generate():
    manifest = []
    for mode in ('ts', 'fmp4'):
        folder = EXTRA / mode
        folder.mkdir(parents=True, exist_ok=True)
        cmd = ['ffmpeg', '-y', '-v', 'error', '-f', 'lavfi', '-i', 'sine=frequency=880:sample_rate=44100',
               '-t', '7.3', '-c:a', 'aac', '-b:a', '64k', '-f', 'hls', '-hls_time', '1.3', '-hls_list_size', '0',
               '-hls_segment_type', 'mpegts' if mode == 'ts' else 'fmp4']
        if mode == 'fmp4': cmd += ['-hls_fmp4_init_filename', 'init.fmp4']
        cmd += ['-hls_segment_filename', str(folder / ('seg%d.ts' if mode == 'ts' else 'seg%d.m4s')), str(folder / 'input.m3u8')]
        run(*cmd)
        manifest.append({'mode': mode, 'command': [arg.replace(str(ROOT), '<repo>') for arg in cmd],
                         'sha256': {p.name: hashlib.sha256(p.read_bytes()).hexdigest() for p in folder.iterdir()}})
    (EXTRA / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')

def verify(video, audio, output, vm, am):
    selected = {'video': packets(video)['video'], 'audio': packets(audio)['audio']}
    actual = packets(output)
    assert actual.keys() == selected.keys()
    origin = min(Fraction(p['dts']) * Fraction(s['time_base']) for rows in selected.values() for p, s in rows)
    for kind, rows in selected.items():
        assert len(rows) == len(actual[kind]), (kind, len(rows), len(actual[kind]))
        mode = am if kind == 'audio' else vm
        for i, ((p, s), (q, t)) in enumerate(zip(rows, actual[kind])):
            tick = Fraction(t['time_base'])
            for field in ('dts', 'pts'):
                expected = int(p[field]) * Fraction(s['time_base']) - origin
                found = int(q[field]) * tick
                assert abs(expected - found) <= tick, (output.name, kind, i, field, expected, found, tick)
            assert normalized_payload(p, s, mode) == normalized_payload(q, t, 'fmp4'), (kind, i, 'payload')
            duration = int(p['duration']) * Fraction(s['time_base'])
            if kind == 'video' and mode == 'ts' and i + 1 < len(rows):
                duration = (int(rows[i+1][0]['dts']) - int(p['dts'])) * Fraction(s['time_base'])
            assert abs(int(q['duration']) * tick - duration) <= tick, (kind, i, 'duration')
    decode_and_seek(output)
    assert frame_hashes(output) == frame_hashes(video), (output.name, 'decoded video tail differs')

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--generate', action='store_true')
    parser.add_argument('--ffmpeg', action='store_true')
    args = parser.parse_args()
    if args.generate: generate()
    for case in json.loads((EXTRA / 'manifest.json').read_text()):
        for name, digest in case['sha256'].items():
            assert hashlib.sha256((EXTRA / case['mode'] / name).read_bytes()).hexdigest() == digest
    build = ['cargo', 'build', '--offline', '--example', 'prepared_demo']
    if args.ffmpeg: build += ['--features', 'ffmpeg-finalize']
    run(*build)
    with tempfile.TemporaryDirectory(prefix='hls-multi-') as tmp:
        for video in ('ts_avc_regular', 'ts_avc_vfr', 'ts_hevc_regular', 'fmp4_avc_regular', 'fmp4_hevc_regular', 'fmp4_avc_negative_cts'):
            vm = video.split('_')[0]
            for am in ('ts', 'fmp4'):
                vp, ap = CORPUS / video / 'input.m3u8', EXTRA / am / 'input.m3u8'
                for mode in ('bytes', 'fragmented', 'native') + (('ffmpeg',) if args.ffmpeg else ()):
                    out = pathlib.Path(tmp) / f'{video}-{am}-{mode}.mp4'
                    run(str(ROOT / 'target/debug/examples/prepared_demo'), str(vp), str(ap), str(out), mode)
                    verify(vp, ap, out, vm, am)
                    print(f'{video} + {am}/880Hz/44.1kHz → {mode}: passed', flush=True)

if __name__ == '__main__': main()
