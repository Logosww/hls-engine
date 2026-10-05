#!/usr/bin/env python3
"""Issue #1: independently generated B frames must survive every keyed output API.

Requires FFmpeg with libx264/libx265, FFprobe, OpenSSL and Cargo. No downloads.
"""
import tempfile
from pathlib import Path
from verify_media import ROOT, run, verify_input, has_signed_cts, frame_hashes


def main():
    run('cargo', 'build', '--offline', '--example', 'keyed_decode_export')
    exporter = str(ROOT / 'target/debug/examples/keyed_decode_export')
    outputs = 0
    with tempfile.TemporaryDirectory(prefix='hls-keyed-decode-') as tmp:
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
                for api in ('bytes', 'file', 'stream'):
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
    print(f'Verified {outputs} outputs with normal edit-list handling.')


if __name__ == '__main__':
    main()
