#!/usr/bin/env python3
"""Independently generated B frames must survive every timeline output API.

Requires FFmpeg with libx264/libx265, FFprobe, OpenSSL and Cargo. No downloads.
"""
import argparse
import shutil
import tempfile
from fractions import Fraction
from pathlib import Path
from verify_media import ROOT, run, verify_input, has_signed_cts, frame_hashes, packets, normalized_payload


def verify_range(playlist, output, mode):
    source, target = packets(playlist), packets(output)
    selected = {}
    for kind, rows in target.items():
        expected = [normalized_payload(p, s, mode) for p, s in source[kind]]
        actual = [normalized_payload(p, s, 'fmp4') for p, s in rows]
        starts = [i for i in range(len(expected) - len(actual) + 1)
                  if expected[i:i + len(actual)] == actual]
        assert len(starts) == 1, (kind, 'output is not an unchanged source interval', starts)
        selected[kind] = source[kind][starts[0]:starts[0] + len(actual)]
    origin = min(Fraction(p['dts']) * Fraction(s['time_base'])
                 for rows in selected.values() for p, s in rows)
    for kind, rows in selected.items():
        for (before, sa), (after, sb) in zip(rows, target[kind]):
            tick = Fraction(sb['time_base'])
            for field in ('dts', 'pts'):
                expected = Fraction(before[field]) * Fraction(sa['time_base']) - origin
                assert abs(Fraction(after[field]) * tick - expected) <= tick, (kind, field)
    # Decode from the new independent entry point; compare every retained video frame.
    expected = frame_hashes(playlist, 'video')
    actual = frame_hashes(output, 'video')
    assert actual and any(expected[i:i + len(actual)] == actual for i in range(len(expected)))
    assert len(actual) <= len(expected)
    assert 'K' in target['video'][0][0]['flags']
    # AAC overlap state at a new cut differs from a decoder run from source EOF;
    # packet identity/timing above and successful complete decode are both required.
    assert frame_hashes(output, 'audio')


def verify_discontinuous(exporter, root, ffmpeg):
    """Decode each output independently, including codec changes and real missing media."""
    corpus = ROOT / 'tests/fixtures/media'
    count = 0
    for case in ('gap-ts', 'gap-fmp4', 'config'):
        clear = root / case
        clear.mkdir()
        sources = []
        if case == 'config':
            tags = ['#EXTM3U', '#EXT-X-TARGETDURATION:2']
            for index, name in enumerate(('ts_avc_video_only', 'ts_hevc_regular', 'ts_avc_video_only')):
                if index:
                    tags.append('#EXT-X-DISCONTINUITY')
                uri = f'part-{index}.ts'
                shutil.copy(corpus / name / 'seg0.ts', clear / uri)
                tags += ['#EXTINF:2,', uri]
                sources.append(corpus / name / 'seg0.ts')
            mode = 'ts'
        else:
            mode = case.removeprefix('gap-')
            folder = corpus / f'{mode}_avc_video_only'
            tags = ['#EXTM3U', '#EXT-X-TARGETDURATION:2']
            if mode == 'fmp4':
                shutil.copy(folder / 'init.fmp4', clear / 'init.fmp4')
                tags.append('#EXT-X-MAP:URI="init.fmp4"')
            extension = 'ts' if mode == 'ts' else 'm4s'
            for index in (0, 2):
                if index:
                    tags += ['#EXT-X-GAP', '#EXTINF:2,', f'missing.{extension}']
                uri = f'seg{index}.{extension}'
                shutil.copy(folder / uri, clear / uri)
                tags += ['#EXTINF:2,', uri]
                if mode == 'ts':
                    sources.append(folder / uri)
                else:
                    reference = root / f'{case}-reference-{index}.mp4'
                    reference.write_bytes((folder / 'init.fmp4').read_bytes() + (folder / uri).read_bytes())
                    sources.append(reference)
        (clear / 'media.m3u8').write_text('\n'.join(tags + ['#EXT-X-ENDLIST', '']))
        encrypted = root / f'aes-{case}'
        encrypted.mkdir()
        for path in clear.iterdir():
            if path.suffix == '.m3u8':
                (encrypted / path.name).write_text(path.read_text().replace(
                    '#EXTM3U\n', '#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI="key",IV=0x0\n'))
            else:
                run('openssl', 'enc', '-aes-128-cbc', '-K', '11' * 16, '-iv', '00' * 16,
                    '-in', str(path), '-out', str(encrypted / path.name))
        for source in (clear, encrypted):
            for collapse in ([False] if case == 'config' else [False, True]):
                output = root / f'{source.name}-split-{collapse}'
                run(exporter, str(source), str(output), '--split', *(['--collapse'] if collapse else []))
                for api in ('bytes', 'file', 'stream') + (('file-ffmpeg',) if ffmpeg else ()):
                    files = sorted(output.glob(f'{api}-[0-9]*.mp4'))
                    expected_parts = len(sources) if case == 'config' or (api != 'stream' and not collapse) else 1
                    assert len(files) == expected_parts, (case, api, len(files), expected_parts)
                    for index, target in enumerate(files):
                        references = sources if len(files) == 1 else [sources[index]]
                        expected_packets, expected_frames = {}, {}
                        for reference in references:
                            for kind, rows in packets(reference).items():
                                expected_packets.setdefault(kind, []).extend(rows)
                                expected_frames.setdefault(kind, []).extend(frame_hashes(reference, kind))
                        actual_packets = packets(target)
                        assert actual_packets.keys() == expected_packets.keys(), (case, api, index)
                        for kind, rows in expected_packets.items():
                            actual = actual_packets[kind]
                            assert len(actual) == len(rows), (case, api, index, kind, len(actual), len(rows))
                            assert frame_hashes(target, kind) == expected_frames[kind], (case, api, index, kind, 'decode')
                            for (before, sa), (after, sb) in zip(rows, actual):
                                assert normalized_payload(before, sa, mode) == normalized_payload(after, sb, 'fmp4')
                                before_cts = (int(before['pts']) - int(before['dts'])) * Fraction(sa['time_base'])
                                after_cts = (int(after['pts']) - int(after['dts'])) * Fraction(sb['time_base'])
                                assert abs(before_cts - after_cts) <= Fraction(sb['time_base'])
                        count += 1
                    print(f'{source.name}/{api}/collapse={collapse}: {len(files)} independently decoded suboutputs', flush=True)
    return count


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--ffmpeg', action='store_true', help='Verify the optional FFmpeg finalizer too')
    options = parser.parse_args()
    command = ['cargo', 'build', '--offline', '--example', 'timeline_export']
    if options.ffmpeg:
        command += ['--features', 'ffmpeg-finalize']
    run(*command)
    exporter = str(ROOT / 'target/debug/examples/timeline_export')
    outputs = 0
    with tempfile.TemporaryDirectory(prefix='hls-timeline-') as tmp:
        root = Path(tmp)
        for codec, mode, signed in [('hevc', 'ts', False), ('hevc', 'fmp4', False),
                                    ('avc', 'ts', False), ('avc', 'fmp4', False),
                                    ('avc', 'fmp4', True)]:
            name = f'{codec}-{mode}-signed{signed}'
            clear = root / name
            clear.mkdir()
            args = ['ffmpeg', '-y', '-v', 'error', '-f', 'lavfi', '-i',
                    'testsrc2=size=160x90:rate=10:duration=2',
                    '-itsoffset', '0.12', '-f', 'lavfi', '-i',
                    'sine=frequency=440:sample_rate=48000:duration=2.4',
                    '-c:v', 'libx265' if codec == 'hevc' else 'libx264',
                    '-preset', 'ultrafast', '-pix_fmt', 'yuv420p']
            if codec == 'hevc':
                args += ['-x265-params', 'log-level=error:keyint=10:min-keyint=10:scenecut=0:pools=1:frame-threads=1',
                         '-tag:v', 'hvc1']
            else:
                args += ['-g', '10', '-bf', '2', '-sc_threshold', '0']
            args += ['-c:a', 'aac', '-b:a', '64k', '-f', 'hls', '-hls_time', '1', '-hls_list_size', '0']
            if mode == 'fmp4':
                args += ['-hls_segment_type', 'fmp4', '-hls_fmp4_init_filename', 'init.mp4']
            if signed:
                args += ['-hls_segment_options', 'movflags=+negative_cts_offsets']
            playlist = clear / 'media.m3u8'
            args += ['-hls_segment_filename', str(clear / ('segment-%02d.ts' if mode == 'ts' else 'segment-%02d.m4s')),
                     str(playlist)]
            run(*args)
            if signed:
                assert has_signed_cts(playlist)
            expected = {kind: frame_hashes(playlist, kind) for kind in ('video', 'audio')}
            assert len(expected['video']) == 20
            encrypted = root / f'aes-{name}'
            encrypted.mkdir()
            for path in clear.iterdir():
                if path.suffix == '.m3u8':
                    (encrypted / path.name).write_text(path.read_text().replace(
                        '#EXTM3U\n', '#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI="key",IV=0x0\n'))
                else:
                    run('openssl', 'enc', '-aes-128-cbc', '-K', '11' * 16, '-iv', '00' * 16,
                        '-in', str(path), '-out', str(encrypted / path.name))
            for source in (clear, encrypted):
                folder = root / f'output-{source.name}'
                run(exporter, str(source), str(folder))
                for api in ('bytes', 'file', 'stream') + (('file-ffmpeg',) if options.ffmpeg else ()):
                    output = folder / f'{api}.mp4'
                    for kind in ('video', 'audio'):
                        actual = frame_hashes(output, kind)
                        assert actual == expected[kind], (
                            source.name, api, kind, 'decoded frames', len(actual), 'expected', len(expected[kind]))
                    # Packet PTS/durations check the actual presentation end and A/V offset,
                    # including the independently longer audio tail.
                    verify_input(playlist, output, mode)
                    outputs += 1
                    print(f'{source.name}/{api}: 20/20 matching video frames and complete audio', flush=True)
                ranged = root / f'range-{source.name}'
                run(exporter, str(source), str(ranged), '--range-ms', '1200', '1500')
                for api in ('bytes', 'file', 'stream') + (('file-ffmpeg',) if options.ffmpeg else ()):
                    verify_range(playlist, ranged / f'{api}.mp4', mode)
                    outputs += 1
                    print(f'{source.name}/{api}: range payload, timing and independent decode verified', flush=True)
        outputs += verify_discontinuous(exporter, root, options.ffmpeg)
    print(f'Verified {outputs} outputs with normal edit-list handling.')


if __name__ == '__main__':
    main()
