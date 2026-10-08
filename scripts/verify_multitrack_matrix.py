"""Independent reconstruction of classic edits versus fragmented packet clocks."""
from fractions import Fraction
import json
from verify_multitrack import OUT, boxes, command, decode, probe


def edit_tracks(path):
    moov = next(b for k, b in boxes(path.read_bytes()) if k == b'moov')
    mvhd = next(b for k, b in boxes(moov) if k == b'mvhd')
    at = 20 if mvhd[0] else 12
    scale = int.from_bytes(mvhd[at:at+4], 'big')
    tracks = []
    for k, body in boxes(moov):
        if k != b'trak':
            continue
        edts = next((b for k, b in boxes(body) if k == b'edts'), None)
        if edts is None:
            tracks.append(None)
            continue
        elst = next(b for k, b in boxes(edts) if k == b'elst')
        width = 8 if elst[0] else 4
        count = int.from_bytes(elst[4:8], 'big')
        entries = []
        for i in range(count):
            at = 8 + i * (width * 2 + 4)
            duration = int.from_bytes(elst[at:at+width], 'big')
            time = int.from_bytes(elst[at+width:at+2*width], 'big', signed=True)
            assert elst[at+2*width:at+2*width+4] == b'\x00\x01\x00\x00'
            entries.append((Fraction(duration, scale), time))
        tracks.append(entries)
    return tracks


def packets(metadata, index):
    return [p for p in metadata['packets'] if p['stream_index'] == index]


def signature(packet, base, shift=0, audio=False):
    return (Fraction(int(packet['pts']))*base+shift,
            # Classic AAC uses compact DTS plus composition offsets for gaps.
            # Presentation, duration and payload must still match exactly.
            None if audio else Fraction(int(packet['dts']))*base+shift,
            Fraction(int(packet['duration']))*base, packet['data_hash'])


def verify(rows):
    evidence = []
    by_name = {r['name']: r for r in rows}
    for row in rows:
        name = row['name']
        if not name.startswith('matrix-') or not name.endswith('-clear-classic'):
            continue
        classic = OUT/(name+'.mp4')
        fragmented = OUT/(name.replace('-classic', '-fragmented')+'.mp4')
        raw = json.loads(command('ffprobe', '-v', 'error', '-ignore_editlist', '1',
            '-show_streams', '-show_packets', '-show_data_hash', 'sha256', '-of', 'json', classic))
        frag = probe(fragmented)
        edits = edit_tracks(classic)
        default = probe(classic)
        default_equal = True
        track_rows = []
        for i, (stream, entries) in enumerate(zip(raw['streams'], edits)):
            base = Fraction(stream['time_base'])
            audio = stream['codec_type'] == 'audio'
            source = packets(raw, i)
            mapped = []
            if entries is None:
                mapped = [signature(p, base, audio=audio) for p in source]
            else:
                cursor = Fraction(0)
                for duration, media_time in entries:
                    if media_time != -1:
                        start = media_time * base
                        selected = [p for p in source if start <= int(p['pts'])*base < start+duration]
                        mapped.extend(signature(p, base, cursor-start, audio=audio) for p in selected)
                    cursor += duration
            expected = [signature(p, Fraction(frag['streams'][i]['time_base']), audio=audio) for p in packets(frag, i)]
            default_packets = packets(default, i)
            default_signatures = [signature(p, Fraction(default['streams'][i]['time_base']), audio=audio) for p in default_packets]
            default_equal = default_equal and sorted(default_signatures) == sorted(expected)
            assert sorted(mapped) == sorted(expected), (name, i, 'edit reconstruction differs from tfdt/trun')
            assert len(mapped) == len(source), (name, i, 'dropped or duplicated samples')
            if audio:
                assert default_signatures == expected, (name, i, 'default AAC presentation/payload mismatch')
                for before, after in zip(default_packets, default_packets[1:]):
                    assert int(after['dts']) == int(before['dts']) + int(before['duration']), (name, i, 'non-contiguous AAC decode run')
            # AAC must decode correctly with the default edit-list handling.
            # Video still uses interior edits, so check its payload integrity
            # separately from the default player timeline reported above.
            if stream['codec_type'] in ('audio', 'video'):
                a = decode(classic, i, stream, source, ignore_edits=not audio)
                b = decode(fragmented, i, frag['streams'][i], packets(frag, i))
                assert a == b, (name, i, 'decoded sample mismatch')
            track_rows.append({'id': stream['id'], 'packets': len(source),
                               'edits': len(entries or []), 'exactPresentationTimelineEqual': True,
                               'decodeTimeline': 'compact' if audio else 'preserved'})
        for suffix in ['classic', 'fragmented']:
            clear = by_name[name.replace('-classic', '-'+suffix)]
            encrypted = by_name[clear['name'].replace('-clear-', '-encrypted-')]
            assert encrypted['hash'] == clear['hash']
        evidence.append({'name': name.removesuffix('-clear-classic'), 'tracks': track_rows,
                         'encryptedClearEqual': True, 'decodedSampleEqual': True, 'ffmpegDefaultPresentationTimelineEqual': default_equal})
    assert len(evidence) == 40
    return evidence


def verify_splits(rows):
    evidence = []
    by_name = {r['name']: r for r in rows}
    for row in rows:
        name = row['name']
        if not name.startswith('split-') or '-clear-' not in name:
            continue
        encrypted = by_name[name.replace('-clear-', '-encrypted-')]
        assert row['hash'] == encrypted['hash']
        path = OUT/(name+'.mp4')
        metadata = probe(path)
        assert len(metadata['streams']) == 4
        assert metadata['streams'][0]['codec_name'] == ('h264' if name.endswith('-0') else 'hevc')
        for stream in metadata['streams']:
            i = stream['index']
            selected = packets(metadata, i)
            assert selected
            if stream['codec_type'] in ('video', 'audio'):
                assert decode(path, i, stream, selected)
            else:
                payloads = [path.read_bytes()[int(p['pos']):int(p['pos'])+int(p['size'])] for p in selected]
                assert any(b'spanning split' in b for b in payloads)
                assert all(b'spanning split' in b or b == b'\x00\x00\x00\x08vtte' for b in payloads)
        evidence.append({'name': name, 'tracks': [s['id'] for s in metadata['streams']],
                         'encryptedClearEqual': True, 'decoded': True})
    assert len(evidence) == 8
    return evidence
