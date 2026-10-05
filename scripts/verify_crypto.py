#!/usr/bin/env python3
"""P3 independently encrypted corpus. Public keys and repository MIT synthetic media only."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / 'tests/fixtures/crypto'
KEY_A = '2b7e151628aed2a6abf7158809cf4f3c'
KEY_B = '603deb1015ca71be2b73aef0857d7781'
IV = '000102030405060708090a0b0c0d0e0f'
SEQUENCE = 9007199254740993
CASES = ['ts_avc_regular', 'ts_hevc_regular', 'ts_aac_audio_only',
         'fmp4_avc_regular', 'fmp4_hevc_regular', 'fmp4_aac_audio_only',
         'ts_avc_video_only', 'fmp4_avc_video_only']


def run(args, data=None):
    return subprocess.run(args, input=data, check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE).stdout


def crypt(data, key, iv, decrypt=False, padding=True):
    assert len(key) == 32 and len(iv) == 32, 'AES-128 key and IV must each be exactly 16 bytes'
    args = ['openssl', 'enc', '-aes-128-cbc', '-K', key, '-iv', iv]
    if decrypt:
        args.append('-d')
    if not padding:
        args.append('-nopad')
    return run(args, data)


def digest(data):
    return hashlib.sha256(data).hexdigest()


def generate():
    OUT.mkdir(exist_ok=True)
    manifest = {'license': 'MIT; synthetic media retained in tests/fixtures/media; public test keys only',
                'openssl': run(['openssl', 'version']).decode().strip(),
                'generator': 'scripts/verify_crypto.py --generate',
                'clear_provenance': 'tests/fixtures/media/manifest.json',
                'encryption_command': 'openssl enc -aes-128-cbc -K KEY -iv IV (stdin plaintext; PKCS7 unless -nopad)',
                'key_a': KEY_A, 'key_b': KEY_B, 'sequence': str(SEQUENCE),
                'files': [], 'cases': []}

    def save(path, data):
        target = OUT / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(data)
        manifest['files'].append({'file': path, 'bytes': len(data), 'sha256': digest(data)})

    for name in CASES:
        folder = ROOT / 'tests/fixtures/media' / name
        fmp4 = name.startswith('fmp4')
        suffix = 'm4s' if fmp4 else 'ts'
        records, lines, range_lines = [], ['#EXTM3U', '#EXT-X-TARGETDURATION:4', f'#EXT-X-MEDIA-SEQUENCE:{SEQUENCE}'], []
        range_lines = lines.copy()
        if fmp4:
            clear_path = f'tests/fixtures/media/{name}/init.fmp4'
            encrypted = crypt((ROOT / clear_path).read_bytes(), KEY_A, IV)
            save(f'{name}/init.cbc', encrypted)
            save(f'{name}/map-range.bin', b'PREFIX!' + encrypted)
            key_line = f'#EXT-X-KEY:METHOD=AES-128,URI="key.bin",IV=0x{IV}'
            lines += [key_line, '#EXT-X-MAP:URI="init.cbc"']
            range_lines += [key_line, f'#EXT-X-MAP:URI="map-range.bin",BYTERANGE="{len(encrypted)}@7"']
            records.append({'kind': 'map', 'cipher': f'{name}/init.cbc', 'clear': clear_path, 'key': KEY_A, 'iv': IV, 'clear_sha256': digest((ROOT / clear_path).read_bytes())})
        bundle = b''
        for i in range(3):
            clear_path = f'tests/fixtures/media/{name}/seg{i}.{suffix}'
            clear = (ROOT / clear_path).read_bytes()
            if i < 2:
                key = KEY_A if i == 0 else KEY_B
                iv = SEQUENCE.to_bytes(16, 'big').hex() if i == 0 else IV
                key_line = '#EXT-X-KEY:METHOD=AES-128,URI="key.bin"' + (f',IV=0x{iv}' if i == 1 else '')
                payload = crypt(clear, key, iv)
                file = f'seg{i}.cbc'
                save(f'{name}/{file}', payload)
                records.append({'kind': 'media', 'index': i, 'cipher': f'{name}/{file}', 'clear': clear_path, 'key': key, 'iv': iv, 'clear_sha256': digest(clear)})
            else:
                key_line = '#EXT-X-KEY:METHOD=NONE'
                file = 'clear.bin'
                payload = clear
                # Clear bytes are read from the retained source corpus, no duplicate copy.
                records.append({'kind': 'media', 'index': i, 'clear': clear_path, 'clear_sha256': digest(clear)})
            lines += [key_line, '#EXTINF:2,', file]
            range_lines += [key_line, '#EXTINF:2,', f'#EXT-X-BYTERANGE:{len(payload)}@{len(bundle)}', 'bundle.bin']
            bundle += payload
        save(f'{name}/bundle.bin', bundle)
        save(f'{name}/input.m3u8', ('\n'.join(lines + ['#EXT-X-ENDLIST', ''])).encode())
        save(f'{name}/range.m3u8', ('\n'.join(range_lines + ['#EXT-X-ENDLIST', ''])).encode())
        manifest['cases'].append({'name': name, 'container': 'fmp4' if fmp4 else 'ts', 'resources': records})
    vector = bytes.fromhex('6bc1bee22e409f96e93d7e117393172aae2d8a571e03ac9c9eb76fac45af8e5130c81c46a35ce411e5fbc1191a0a52eff69f2445df4f9b17ad2b417be66c3710')
    save('vector.cbc', crypt(vector, KEY_A, IV))
    save('invalid-padding.cbc', crypt(bytes(16), KEY_A, IV, padding=False))
    save('invalid-container.cbc', crypt(b'not a TS or fMP4 resource', KEY_A, IV))
    (OUT / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')


def verify(openssl=False, ffmpeg=False):
    manifest = json.loads((OUT / 'manifest.json').read_text())
    for file in manifest['files']:
        data = (OUT / file['file']).read_bytes()
        assert len(data) == file['bytes'] and digest(data) == file['sha256'], file['file']
    decrypted = 0
    for case in manifest['cases']:
        combined = b''
        for resource in case['resources']:
            clear = (ROOT / resource['clear']).read_bytes()
            assert digest(clear) == resource['clear_sha256'], resource['clear']
            if 'cipher' in resource and (openssl or ffmpeg):
                actual = crypt((OUT / resource['cipher']).read_bytes(), resource['key'], resource['iv'], decrypt=True)
                assert actual == clear, resource['cipher']
                decrypted += 1
                combined += actual
            else:
                combined += clear
        if ffmpeg:
            with tempfile.NamedTemporaryFile(suffix='.mp4' if case['container'] == 'fmp4' else '.ts') as file:
                file.write(combined)
                file.flush()
                run(['ffmpeg', '-v', 'error', '-i', file.name, '-f', 'null', '-'])
    print(json.dumps({'fixtures': len(manifest['files']), 'cases': len(manifest['cases']), 'independent_decryptions': decrypted, 'decoded_cases': len(manifest['cases']) if ffmpeg else 0}))


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--generate', action='store_true')
    parser.add_argument('--openssl', action='store_true')
    parser.add_argument('--ffmpeg', action='store_true')
    args = parser.parse_args()
    if args.generate:
        generate()
    verify(args.openssl, args.ffmpeg)
