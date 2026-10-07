#!/usr/bin/env python3
"""Generate MIT synthetic Packed AAC fixtures with independent FFmpeg/OpenSSL tools."""
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
DEST = ROOT / 'tests/fixtures/packed_aac'
KEY = '2b7e151628aed2a6abf7158809cf4f3c'
IV = '00000000000000000000000000000001'
KEY2 = '603deb1015ca71be2b73aef0857d7781'


def run(*args, data=None):
    return subprocess.check_output(args, input=data)


def syncsafe(n):
    return bytes((n >> s) & 127 for s in (21, 14, 7, 0))


def anchor(ticks):
    payload = b'com.apple.streaming.transportStreamTimestamp\0' + ticks.to_bytes(8, 'big')
    frame = b'PRIV' + syncsafe(len(payload)) + b'\0\0' + payload
    return b'ID3\x04\0\0' + syncsafe(len(frame)) + frame


def encrypt(data, padding=True, key=KEY):
    command = ['openssl', 'enc', '-aes-128-cbc', '-K', key, '-iv', IV]
    if not padding:
        command.append('-nopad')
    return run(*command, data=data)


def main():
    DEST.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory() as tmp:
        raw = Path(tmp) / 'audio.adts'
        subprocess.run(['ffmpeg', '-v', 'error', '-f', 'lavfi', '-i',
                        'sine=frequency=440:sample_rate=44100:duration=1.2', '-ac', '2',
                        '-c:a', 'aac', '-profile:a', 'aac_low', '-b:a', '64k',
                        '-flags', '+bitexact', '-fflags', '+bitexact', '-f', 'adts', str(raw)], check=True)
        data = raw.read_bytes()
    frames = []
    while data:
        n = ((data[3] & 3) << 11) | (data[4] << 3) | (data[5] >> 5)
        assert n >= 7 and n <= len(data)
        frames.append(data[:n])
        data = data[n:]
    manifest = {'license': 'MIT', 'source': 'FFmpeg synthetic sine, 440 Hz, 44100 Hz stereo AAC-LC',
                'ffmpeg': run('ffmpeg', '-version').decode().splitlines()[0],
                'openssl': run('openssl', 'version').decode().strip(), 'key': KEY, 'rotatedKey': KEY2, 'iv': IV,
                'sampleRate': 44100, 'frameSamples': 1024, 'frames': len(frames), 'files': {}}
    for profile in ('clear', 'aes128', 'sample_aes', 'aes128_rotation', 'sample_aes_rotation'):
        folder = DEST / profile
        folder.mkdir(exist_ok=True)
        playlist = ['#EXTM3U', '#EXT-X-VERSION:5', '#EXT-X-TARGETDURATION:1']
        if profile != 'clear':
            method = 'AES-128' if profile.startswith('aes128') else 'SAMPLE-AES'
            playlist.append(f'#EXT-X-KEY:METHOD={method},URI="key",IV=0x{IV}')
        for index, start in enumerate(range(0, len(frames), 17)):
            key=KEY2 if profile.endswith('_rotation') and index>=2 else KEY
            if profile.endswith('_rotation') and index==2:
                playlist.append(f'#EXT-X-KEY:METHOD={method},URI="packed-rotated-key",IV=0x{IV}')
            selected = frames[start:start + 17]
            if profile.startswith('sample_aes'):
                protected = []
                for frame in selected:
                    # ADTS stays clear, first 16 AAC bytes stay clear, CBC resets per frame.
                    head = 7 if frame[1] & 1 else 9
                    payload = frame[head:]
                    count = max(0, (len(payload) - 16) // 16) * 16
                    encrypted = encrypt(payload[16:16+count], False, key) if count else b''
                    protected.append(frame[:head] + payload[:16] + encrypted + payload[16+count:])
                selected = protected
            resource = anchor(start * 1024 * 90000 // 44100) + b''.join(selected)
            if profile.startswith('aes128'):
                resource = encrypt(resource, key=key)
            name = f'seg{index}.bin'
            (folder / name).write_bytes(resource)
            playlist.extend((f'#EXTINF:{len(selected) * 1024 / 44100:.9f},', name))
        playlist.append('#EXT-X-ENDLIST')
        (folder / 'input.m3u8').write_text('\n'.join(playlist) + '\n')
    for file in sorted(DEST.glob('*/*')):
        manifest['files'][str(file.relative_to(DEST))] = hashlib.sha256(file.read_bytes()).hexdigest()
    (DEST / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')


if __name__ == '__main__':
    main()
