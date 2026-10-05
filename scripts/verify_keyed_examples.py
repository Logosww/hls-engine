#!/usr/bin/env python3
"""Run the native key-file example against retained public-key AES fixtures."""
import json
import tempfile
from pathlib import Path
from verify_media import ROOT, run, decode_and_seek

run('cargo', 'build', '--offline', '--example', 'keyed_demo')
with tempfile.TemporaryDirectory(prefix='keyed-example-') as tmp:
    folder=Path(tmp)
    encrypted=ROOT/'tests/fixtures/crypto/ts_avc_regular'
    lines=(encrypted/'input.m3u8').read_text().splitlines()
    key=0
    for i,line in enumerate(lines):
        if 'URI="key.bin"' in line:
            lines[i]=line.replace('key.bin',f'key-{key}.bin')
            key+=1
    (folder/'input.m3u8').write_text('\n'.join(lines)+'\n')
    for i,secret in enumerate(['2b7e151628aed2a6abf7158809cf4f3c','603deb1015ca71be2b73aef0857d7781']):
        (folder/f'key-{i}.bin').write_bytes(bytes.fromhex(secret))
        (folder/f'seg{i}.cbc').write_bytes((encrypted/f'seg{i}.cbc').read_bytes())
    (folder/'clear.bin').write_bytes((ROOT/'tests/fixtures/media/ts_avc_regular/seg2.ts').read_bytes())
    output=folder/'output.mp4'
    result=run(str(ROOT/'target/debug/examples/keyed_demo'),str(folder/'input.m3u8'),str(output))
    assert 'committed=3' in result
    decode_and_seek(output)
    print(json.dumps({'native_example':True,'committed_segments':3,'decoded_and_seeked':True}))
