#!/usr/bin/env python3
"""Generate a continuous B-frame HLS input and independently verify Native finalize.
Run: python3 scripts/verify_media.py (requires ffmpeg, ffprobe, cargo).
"""
import json
import pathlib
import struct
import subprocess
import tempfile
import re
from fractions import Fraction

ROOT = pathlib.Path(__file__).resolve().parents[1]


def run(*args):
    return subprocess.check_output(args, cwd=ROOT, stderr=subprocess.PIPE).decode()


def packets(path):
    doc = json.loads(run('ffprobe', '-v', 'error', '-show_streams', '-show_packets',
                         '-show_data', '-show_data_hash', 'sha256', '-of', 'json', str(path)))
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


def payload(packet):
    chunks = []
    for line in packet['data'].splitlines():
        if ': ' not in line:
            continue
        chunks.append(bytes.fromhex(line.split(': ', 1)[1].split('  ', 1)[0].replace(' ', '')))
    return b''.join(chunks)


def normalized_payload(packet, stream, mode):
    data = payload(packet)
    if stream['codec_type'] == 'audio':
        if mode == 'ts':
            header = 7 if data[1] & 1 else 9
            data = data[header:]
        return data
    if mode == 'ts':
        nals = tuple(n.rstrip(b'\x00') for n in re.split(b'\x00\x00\x00?\x01', data) if n)
        return tuple(n for n in nals if not parameter_nal(n, stream))
    nals = []
    while data:
        width = int(stream.get('nal_length_size', 4))
        length = int.from_bytes(data[:width], 'big')
        assert length > 0 and length + width <= len(data)
        nals.append(data[width:width+length])
        data = data[width+length:]
    return tuple(n for n in nals if not parameter_nal(n, stream))


def parameter_nal(nal, stream):
    if stream['codec_name'] == 'h264':
        return nal[0] & 31 in (7, 8)
    return (nal[0] >> 1) & 63 in (32, 33, 34)


def verify_input(playlist, output, mode):
    source, target = packets(playlist), packets(output)
    assert source.keys() == target.keys()
    origin = min(Fraction(p['dts']) * Fraction(s['time_base']) for rows in source.values() for p, s in rows)
    for kind, rows in source.items():
        assert len(rows) == len(target[kind]), (kind, len(rows), len(target[kind]))
        for index, ((before, sa), (after, sb)) in enumerate(zip(rows, target[kind])):
            tick = Fraction(sb['time_base'])
            for field in ('dts', 'pts'):
                expected = Fraction(before[field]) * Fraction(sa['time_base']) - origin
                actual = Fraction(after[field]) * tick
                assert abs(actual - expected) <= tick, (mode, kind, index, field, float(expected), float(actual))
            if mode == 'ts' and kind == 'video' and index + 1 < len(rows):
                duration = (int(rows[index+1][0]['dts']) - int(before['dts'])) * Fraction(sa['time_base'])
            else:
                duration = int(before['duration']) * Fraction(sa['time_base'])
            assert abs(int(after['duration']) * tick - duration) <= tick, (mode, kind, index, 'duration', duration, after['duration'])
            assert normalized_payload(before, sa, mode) == normalized_payload(after, sb, 'fmp4'), (mode, kind, index, 'NAL/payload mismatch')
        print(f'{mode} input → output: {len(rows)} {kind} samples, timestamps/duration/normalized payload verified')


def boxes(data, start=0, end=None):
    end = len(data) if end is None else end
    while start < end:
        size, kind = struct.unpack_from('>I4s', data, start)
        assert size >= 8 and start + size <= end
        yield start, size, kind
        start += size


def rewrite_nal2_and_runs(playlist, width=2):
    """Derive independently readable avc3/two-byte NAL/multi-trun input."""
    init = playlist.parent / re.search(r'URI="([^"]+)"', playlist.read_text())[1]
    data = bytearray(init.read_bytes())
    avcc = data.index(b'avcC')
    data[avcc+8] = (data[avcc+8] & 0xfc) | (width-1)
    init.write_bytes(data.replace(b'avc1', b'avc3'))
    for uri in playlist.read_text().splitlines():
        if not uri or uri.startswith('#'):
            continue
        path = playlist.parent / uri
        data = bytearray(path.read_bytes())
        moof, moof_size, _ = next(box for box in boxes(data) if box[2] == b'moof')
        mdat, mdat_size, _ = next(box for box in boxes(data) if box[2] == b'mdat')
        changes, offsets = [], []
        for traf, size, kind in boxes(data, moof+8, moof+moof_size):
            if kind != b'traf':
                continue
            children = list(boxes(data, traf+8, traf+size))
            tfhd = next(box[0] for box in children if box[2] == b'tfhd')
            video = struct.unpack_from('>I', data, tfhd+12)[0] == 1
            for run, run_size, kind in children:
                if kind != b'trun':
                    continue
                flags, count = struct.unpack_from('>II', data, run+8)
                flags &= 0xffffff
                assert flags & 1
                offsets.append(run+16)
                cursor = run+20 + (4 if flags & 4 else 0)
                media = moof + struct.unpack_from('>i', data, run+16)[0]
                fields = [flag for flag in (0x100, 0x200, 0x400, 0x800) if flags & flag]
                if video:
                    assert 0x200 in fields
                    for _ in range(count):
                        size_pos = cursor + fields.index(0x200)*4
                        sample_size = struct.unpack_from('>I', data, size_pos)[0]
                        old = bytes(data[media:media+sample_size])
                        new = b''
                        while old:
                            n = int.from_bytes(old[:4], 'big')
                            assert n > 0
                            if n >= 1 << (8*width) and width == 1 and old[4] & 31 == 6:
                                old = old[4+n:]  # encoder-identification SEI; retain all coded media
                                continue
                            assert n < 1 << (8*width)
                            new += n.to_bytes(width, 'big') + old[4:4+n]
                            old = old[4+n:]
                        changes.append((media, media+sample_size, new))
                        struct.pack_into('>I', data, size_pos, len(new))
                        media += sample_size
                        cursor += 4*len(fields)
        changes.sort()
        delta = sum(len(new) - (end-start) for start, end, new in changes)
        for offset in offsets:
            old = moof + struct.unpack_from('>i', data, offset)[0]
            adjustment = sum(len(new) - (end-start) for start, end, new in changes if end <= old)
            struct.pack_into('>i', data, offset, old-moof+adjustment)
        for start, size, kind in boxes(data):
            if kind == b'sidx':
                ref = start+32 if data[start+8] == 0 else start+40
                old = struct.unpack_from('>I', data, ref)[0]
                struct.pack_into('>I', data, ref, old+delta)
        struct.pack_into('>I', data, mdat, mdat_size+delta)
        for start, end, new in reversed(changes):
            data[start:end] = new
        # Split the first video's run; second run continues at the previous end.
        moof_payload = b''
        growth = 0
        for traf, size, kind in boxes(data, moof+8, moof+moof_size):
            if kind != b'traf':
                moof_payload += data[traf:traf+size]
                continue
            children = list(boxes(data, traf+8, traf+size))
            tfhd = next(box[0] for box in children if box[2] == b'tfhd')
            payload = b''
            video = struct.unpack_from('>I', data, tfhd+12)[0] == 1
            for run, run_size, kind in children:
                if kind != b'trun' or not video:
                    payload += data[run:run+run_size]
                    continue
                vf, count = struct.unpack_from('>II', data, run+8)
                header = 8 + (4 if vf & 1 else 0) + (4 if vf & 4 else 0)
                stride = 4*sum(bool(vf & flag) for flag in (0x100,0x200,0x400,0x800))
                cut = count//2
                assert cut > 0
                first = bytearray(data[run+8:run+8+header+cut*stride])
                struct.pack_into('>I', first, 4, cut)
                fields = [flag for flag in (0x100,0x200,0x400,0x800) if vf & flag]
                first_bytes = sum(struct.unpack_from('>I', first, header+i*stride+fields.index(0x200)*4)[0] for i in range(cut))
                second_offset = struct.unpack_from('>i', first, 8)[0] + first_bytes
                second = struct.pack('>IIi', vf & ~4, count-cut, second_offset) + data[run+8+header+cut*stride:run+run_size]
                one = struct.pack('>I4s', len(first)+8, b'trun') + first
                two = struct.pack('>I4s', len(second)+8, b'trun') + second
                payload += one + two
                growth += len(one)+len(two)-run_size
            moof_payload += struct.pack('>I4s', len(payload)+8, b'traf') + payload
        new_moof = bytearray(struct.pack('>I4s', len(moof_payload)+8, b'moof')+moof_payload)
        for traf, size, kind in boxes(new_moof, 8):
            if kind != b'traf':
                continue
            for run, run_size, kind in boxes(new_moof, traf+8, traf+size):
                if kind == b'trun' and new_moof[run+11] & 1:
                    struct.pack_into('>i', new_moof, run+16, struct.unpack_from('>i', new_moof, run+16)[0]+growth)
        for start, size, kind in boxes(data):
            if kind == b'sidx':
                ref = start+32 if data[start+8] == 0 else start+40
                struct.pack_into('>I', data, ref, struct.unpack_from('>I', data, ref)[0]+growth)
        data[moof:moof+moof_size] = new_moof
        path.write_bytes(data)


def main():
    run('cargo', 'build', '--offline', '--example', 'transmux_demo')
    with tempfile.TemporaryDirectory(prefix='hls-transmux-media-') as directory:
        folder = pathlib.Path(directory)
        cases = [('ts', 'avc', 'regular'), ('ts', 'avc', 'vfr'), ('fmp4', 'avc', 'regular'), ('fmp4', 'avc', 'negative_cts'),
                 ('fmp4', 'avc', 'vfr'), ('fmp4', 'avc', 'offset'), ('fmp4', 'avc', 'nal2_multirun'), ('fmp4', 'avc', 'nal1_multirun'),
                 ('fmp4', 'hevc', 'regular')]
        for mode, codec, scenario in cases:
            name = f'{mode}_{codec}_{scenario}'
            playlist = folder / f'input_{name}.m3u8'
            audio_offset = ['-itsoffset', '0.12'] if scenario == 'offset' else []
            video = ['-c:v', 'libx264', '-g', '60', '-bf', '2', '-sc_threshold', '0'] if codec == 'avc' else [
                '-c:v', 'libx265', '-x265-params', 'pools=1:frame-threads=1:log-level=error:keyint=60:min-keyint=60:scenecut=0', '-tag:v', 'hev1']
            filtering = ['-vf', "select='not(eq(mod(n,5),0))'", '-fps_mode', 'vfr'] if scenario == 'vfr' else []
            fragment_options = ['-hls_segment_options', 'movflags=+negative_cts_offsets'] if scenario == 'negative_cts' else []
            picture = 'color=c=black:size=16x16:rate=30' if scenario == 'nal1_multirun' else 'testsrc2=size=320x180:rate=30'
            run('ffmpeg', '-v', 'error', '-f', 'lavfi', '-i', picture,
                *audio_offset, '-f', 'lavfi', '-i', 'sine=frequency=440:sample_rate=48000', '-t', '6',
                *filtering, *video, '-pix_fmt', 'yuv420p', '-c:a', 'aac', '-b:a', '96k',
                '-f', 'hls', '-hls_time', '2', '-hls_list_size', '0',
                '-hls_segment_type', 'mpegts' if mode == 'ts' else 'fmp4', *fragment_options, str(playlist))
            if scenario in ('nal1_multirun', 'nal2_multirun'):
                rewrite_nal2_and_runs(playlist, 1 if scenario == 'nal1_multirun' else 2)
            fragment = folder / f'fragment_{name}.mp4'
            classic = folder / f'classic_{name}.mp4'
            batch = folder / f'batch_{name}.mp4'
            demo = str(ROOT / 'target/debug/examples/transmux_demo')
            run(demo, str(playlist), str(fragment), '--fragmented')
            run(demo, str(playlist), str(classic), '--streaming')
            run(demo, str(playlist), str(batch), '--batch')
            print(f'Input: {name}')
            verify_input(playlist, fragment, mode)
            verify_input(playlist, classic, mode)
            verify_input(playlist, batch, mode)
            verify(fragment, classic)
            verify(fragment, batch)
            print('Native faststart output decoded and seeked successfully.')



if __name__ == '__main__':
    main()
