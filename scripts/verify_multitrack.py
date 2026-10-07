#!/usr/bin/env python3
"""Independent packet, decoded A/V and wvtt interval checks for v0.10 outputs."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
from fractions import Fraction

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / 'target/multitrack'


def command(*args):
    return subprocess.check_output([str(a) for a in args], cwd=ROOT)


def probe(path):
    return json.loads(command('ffprobe', '-v', 'error', '-show_streams', '-show_packets', '-show_data_hash', 'sha256', '-of', 'json', path))


def decode(path, index, stream, packets, ignore_edits=False):
    args = ['ffmpeg', '-v', 'error', '-xerror', '-err_detect', 'explode']
    if ignore_edits:
        args += ['-ignore_editlist', '1']
    args += ['-i', path, '-map', f'0:{index}']
    if stream['codec_type'] == 'audio':
        count = int(sum(int(p.get('duration', 0)) for p in packets) * Fraction(stream['time_base']) * int(stream['sample_rate']))
        args += ['-af', f'atrim=end_sample={count}']
    else:
        args += ['-fps_mode', 'passthrough']
    lines = command(*args, '-f', 'framemd5', '-').decode().splitlines()
    return [line.split(',')[-1].strip() for line in lines if line and not line.startswith('#')]


def boxes(data):
    pos = 0
    while pos < len(data):
        assert len(data) - pos >= 8
        size = int.from_bytes(data[pos:pos+4], 'big')
        assert 8 <= size <= len(data)-pos
        yield data[pos+4:pos+8], data[pos+8:pos+size]
        pos += size


def subtitles(path, metadata):
    data = path.read_bytes()
    rows = []
    for stream in metadata['streams']:
        if stream['codec_tag_string'] != 'wvtt':
            continue
        packets = [p for p in metadata['packets'] if p['stream_index'] == stream['index']]
        intervals = []
        for packet in packets:
            pos, size = int(packet['pos']), int(packet['size'])
            entries = []
            for kind, body in boxes(data[pos:pos+size]):
                if kind == b'vtte':
                    assert not body
                    continue
                assert kind == b'vttc', kind
                cue = {kind.decode(): payload.decode('utf8') for kind, payload in boxes(body)}
                assert 'payl' in cue
                entries.append(cue)
            intervals.append({'start': int(packet['pts']), 'duration': int(packet['duration']), 'cues': entries})
        assert all(a['start']+a['duration'] == b['start'] for a, b in zip(intervals, intervals[1:])), intervals
        assert any(len(i['cues']) == 2 for i in intervals), intervals
        assert any(not i['cues'] for i in intervals), intervals
        assert any(c.get('sttg') == 'align:start position:10%' for i in intervals for c in i['cues'])
        rows.append({'trackId': stream['id'], 'language': stream['tags']['language'], 'timeBase': stream['time_base'], 'intervals': intervals})
    return rows



def ffmpeg_self_remux(reference):
    # Distinguish an external decoder/edit-list interaction from our mux layout.
    path = OUT/'ffmpeg-self-remux.mp4'
    command('ffmpeg', '-v', 'error', '-y', '-i', OUT/'multitrack-fragmented.mp4',
            '-map', '0:v', '-map', '0:a', '-c', 'copy', path)
    metadata = probe(path)
    rows = []
    for stream in metadata['streams']:
        if stream['codec_type'] != 'audio':
            continue
        index = stream['index']
        packets = [p for p in metadata['packets'] if p['stream_index'] == index]
        rows.append({'track': index, 'defaultDecodeEqual': decode(path, index, stream, packets) == reference,
                     'lastPacketSideData': packets[-1].get('side_data_list', [])})
    return rows


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    manifest = json.loads((ROOT/'tests/fixtures/packed_aac/manifest.json').read_text())
    for name, digest in manifest['files'].items():
        assert hashlib.sha256((ROOT/'tests/fixtures/packed_aac'/name).read_bytes()).hexdigest() == digest
    reports = json.loads(subprocess.check_output(['cargo', 'run', '--offline', '--features', 'serde', '--example', 'multitrack_verify'], cwd=ROOT, env={**os.environ, 'HLS_MULTITRACK_OUTPUT': str(OUT)}))
    (OUT/'reports.json').write_text(json.dumps(reports, indent=2)+'\n')
    references = {}
    for codec in ['avc', 'aac']:
        folder = ROOT/'tests/fixtures/sample_crypto'/f'fmp4_{codec}_clear'
        reference = OUT/f'{codec}.reference'
        reference.write_bytes((folder/'init.mp4').read_bytes()+b''.join(p.read_bytes() for p in sorted(folder.glob('seg*.m4s'))))
        metadata = probe(reference)
        references[codec] = decode(reference, 0, metadata['streams'][0], metadata['packets'])
    # Decode the original ADTS packets, independently of any MP4 output.
    adts = bytearray()
    for path in sorted((ROOT/'tests/fixtures/packed_aac/clear').glob('*.bin')):
        data = path.read_bytes()
        size = 0
        for byte in data[6:10]:
            size = (size << 7) | byte
        adts.extend(data[10+size:])
    reference = OUT/'packed-reference.aac'
    reference.write_bytes(adts)
    metadata = probe(reference)
    references['packed'] = decode(reference, 0, metadata['streams'][0], metadata['packets'])
    evidence = []
    player_limits = []
    text_by_mode = {}
    for row in reports:
        if row['name'].startswith(('matrix-', 'split-')):
            continue
        path = OUT/(row['name']+'.mp4')
        metadata = probe(path)
        streams = metadata['streams']
        frame_count = 0
        for stream in streams:
            if stream['codec_type'] not in ('video', 'audio'):
                continue
            index = stream['index']
            packets = [p for p in metadata['packets'] if p['stream_index'] == index]
            decoded = decode(path, index, stream, packets)
            key = 'avc' if stream['codec_type'] == 'video' else ('packed' if stream.get('sample_rate') == '44100' else 'aac')
            if decoded != references[key]:
                # FFmpeg's own classic remux reproduces this empty-edit tail trim.
                # Record it as a player failure; ignoring edits proves only sample integrity.
                assert stream['codec_type'] == 'audio' and row['name'].endswith('-classic')
                assert decode(path, index, stream, packets, ignore_edits=True) == references[key]
                player_limits.append({'output': row['name'], 'track': index, 'defaultDecodeEqual': False,
                                     'sampleDecodeIgnoringEditsEqual': True, 'startOffsetSamples': stream.get('start_pts'),
                                     'lastPacketSideData': packets[-1].get('side_data_list', [])})
            frame_count += len(decoded)
        if row['name'].startswith('packed-'):
            mode = row['name'].rsplit('-', 1)[1]
            clear = next(r for r in reports if r['name'] == 'packed-clear-'+mode)
            assert row['hash'] == clear['hash'], row['name']
        else:
            audios = [s for s in streams if s['codec_type'] == 'audio']
            assert [s['tags']['language'] for s in audios] == ['eng', 'jpn']
            assert [s['disposition']['default'] for s in audios] == [1, 0]
            assert len({s['id'] for s in streams}) == 5
            text_by_mode[row['name']] = subtitles(path, metadata)
            assert len(text_by_mode[row['name']]) == 2
        evidence.append({'name': row['name'], 'normalizedOutputSha256': row['hash'], 'decodedFrames': frame_count,
                         'packets': len(metadata['packets']), 'clearReferenceEqual': True})
    assert all(text == text_by_mode['multitrack-classic'] for text in text_by_mode.values())
    from verify_multitrack_matrix import verify, verify_splits
    matrix = verify(reports)
    splits = verify_splits(reports)
    result = {'ffmpegSelfRemux': ffmpeg_self_remux(references['aac']), 'splits': splits, 'splitOutputs': len(splits)*2, 'matrix': matrix, 'matrixOutputs': len(matrix)*4, 'status': 'PASS_WITH_PLAYER_LIMITATIONS' if player_limits else 'PASS', 'playerLimits': player_limits, 'ffmpeg': command('ffmpeg', '-version').decode().splitlines()[0],
              'cases': evidence, 'subtitles': text_by_mode, 'playerRendering': 'not tested by this script'}
    (OUT/'evidence.json').write_text(json.dumps(result, indent=2, ensure_ascii=False)+'\n')
    print(f'Multitrack: {len(evidence)} base outputs, {len(matrix)*4} matrix outputs, {len(splits)*2} split outputs verified')


if __name__ == '__main__':
    main()
