#!/usr/bin/env python3
"""Retained independent Shaka Packager fixtures; regeneration is explicit."""
import argparse, hashlib, json, subprocess, os
from pathlib import Path
import sample_fixture_layout
ROOT = Path(__file__).resolve().parents[1]
DEST = ROOT / 'tests/fixtures/sample_crypto'
KEY = '2b7e151628aed2a6abf7158809cf4f3c'
KID = '00112233445566778899aabbccddeeff'
IV = '000102030405060708090a0b0c0d0e0f'

def run(args):
    subprocess.run([str(a) for a in args], check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)

def generate(packager):
    DEST.mkdir(parents=True, exist_ok=True)
    work = ROOT / 'target/sample-fixtures'
    work.mkdir(parents=True, exist_ok=True)
    commands = []
    for codec, source, stream in [('avc','fmp4_avc_video_only','video'), ('hevc','fmp4_hevc_regular','video'), ('aac','fmp4_aac_audio_only','audio')]:
        media = ROOT / 'tests/fixtures/media' / source
        inp = work / (codec + '.mp4')
        inp.write_bytes((media / 'init.fmp4').read_bytes() + b''.join(p.read_bytes() for p in sorted(media.glob('seg*.m4s'))))
        for scheme in ['clear','cenc','cbcs']:
            out = DEST / f'fmp4_{codec}_{scheme}'
            out.mkdir(exist_ok=True)
            args = [packager, f'in={inp},stream={stream},init_segment={out}/init.mp4,segment_template={out}/seg$Number$.m4s,playlist_name=input.m3u8', '--segment_duration','2','--fragment_duration','1','--hls_master_playlist_output',out/'master.m3u8']
            if scheme != 'clear':
                args += ['--enable_raw_key_encryption','--keys',f'label=:key_id={KID}:key={KEY}','--iv',IV,'--clear_lead','0','--protection_scheme',scheme,'--hls_key_uri','key']
            run(args); commands.append([str(a).replace(str(ROOT),'$ROOT').replace(str(packager),'$PACKAGER') for a in args])
    # TS packaging uses Apple's SAMPLE-AES layout (AVC/AAC).
    for scheme in ['clear','sample']:
        out = DEST / f'ts_avc_{scheme}'; out.mkdir(exist_ok=True)
        args = [packager, f'in={work}/avc.mp4,stream=video,segment_template={out}/seg$Number$.ts,playlist_name=input.m3u8', '--segment_duration','2','--hls_master_playlist_output',out/'master.m3u8']
        if scheme == 'sample': args += ['--enable_raw_key_encryption','--keys',f'label=:key_id={KID}:key={KEY}','--iv',IV,'--clear_lead','0','--protection_scheme','cbcs','--hls_key_uri','key']
        run(args); commands.append([str(a).replace(str(ROOT),'$ROOT').replace(str(packager),'$PACKAGER') for a in args])
    for scheme in ['clear','sample']:
        out = DEST / f'ts_aac_{scheme}'; out.mkdir(exist_ok=True)
        args = [packager, f'in={work}/aac.mp4,stream=audio,segment_template={out}/seg$Number$.ts,playlist_name=input.m3u8', '--segment_duration','2','--hls_master_playlist_output',out/'master.m3u8']
        if scheme == 'sample': args += ['--enable_raw_key_encryption','--keys',f'label=:key_id={KID}:key={KEY}','--iv',IV,'--clear_lead','0','--protection_scheme','cbcs','--hls_key_uri','key']
        run(args); commands.append([str(a).replace(str(ROOT),'$ROOT').replace(str(packager),'$PACKAGER') for a in args])
    # Normalize playlist-only packaging tags; preserve every duration and encryption declaration.
    for p in DEST.glob('*/input.m3u8'):
        lines = p.read_text().splitlines()
        if p.parent.name.endswith('_cenc'):
            # Shaka does not emit the deprecated SAMPLE-AES-CTR HLS signaling;
            # the independently generated ISO cenc media is unchanged.
            lines.insert(next(i for i,line in enumerate(lines) if line.startswith('#EXTINF:')), '#EXT-X-KEY:METHOD=SAMPLE-AES-CTR,URI="key",KEYFORMAT="identity"')
        p.write_text('\n'.join(line for line in lines if not line.startswith(('#EXT-X-VERSION:','#EXT-X-INDEPENDENT-SEGMENTS'))) + '\n')
    mixed = DEST/'mixed'; mixed.mkdir(exist_ok=True)
    for source, target in [('init.mp4','aes-init.mp4'), ('seg3.m4s','aes-seg3.m4s')]:
        args = ['openssl','enc','-aes-128-cbc','-K',KEY,'-iv',IV,'-in',str(DEST/'fmp4_avc_clear'/source),'-out',str(mixed/target)]
        run(args); commands.append([a.replace(str(ROOT),'$ROOT') for a in args])
    sample_fixture_layout.generate(ROOT,DEST,KEY,KID)
    manifest = {'generator':subprocess.check_output([packager,'--version'],text=True).strip(), 'generatorSha256':hashlib.sha256(Path(packager).read_bytes()).hexdigest(), 'license':'Synthetic repository MIT media; Shaka Packager Apache-2.0 tool', 'nalWidthGenerator':'scripts/sample_fixture_layout.py with openssl enc -aes-128-ctr -nopad; 8-byte per-sample IV padded with eight zero counter bytes', 'openssl':subprocess.check_output(['openssl','version'],text=True).strip(), 'key':KEY,'kid':KID,'iv':IV,'commands':commands,
        'sourceFiles':{str(p.relative_to(ROOT)):hashlib.sha256(p.read_bytes()).hexdigest() for name in ['fmp4_avc_video_only','fmp4_hevc_regular','fmp4_aac_audio_only','fmp4_avc_nal1_multirun','fmp4_avc_nal2_multirun'] for p in sorted((ROOT/'tests/fixtures/media'/name).iterdir()) if p.is_file()},
        'files':{str(p.relative_to(DEST)):hashlib.sha256(p.read_bytes()).hexdigest() for p in sorted(DEST.glob('*/*')) if p.is_file()}}
    (DEST/'manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')

def main():
    p=argparse.ArgumentParser();p.add_argument('--generate',action='store_true');p.add_argument('--packager',default='/tmp/hls-packager-v3.9.3');p.add_argument('--ffmpeg',action='store_true');p.add_argument('--ffmpeg-finalize',action='store_true');a=p.parse_args()
    if a.generate: generate(a.packager)
    manifest=json.loads((DEST/'manifest.json').read_text())
    for path,digest in manifest['files'].items(): assert hashlib.sha256((DEST/path).read_bytes()).hexdigest()==digest,path
    for path,digest in manifest['sourceFiles'].items(): assert hashlib.sha256((ROOT/path).read_bytes()).hexdigest()==digest,path
    print(f"Verified {len(manifest['files'])} independent sample-encryption files ({manifest['generator']})")
    if a.ffmpeg:
        output=ROOT/'target/sample-output';output.mkdir(parents=True,exist_ok=True)
        command=['cargo','test','--offline','--test','sample_crypto']
        if a.ffmpeg_finalize: command += ['--features','ffmpeg-finalize']
        subprocess.run(command,cwd=ROOT,env={**os.environ,'HLS_SAMPLE_OUTPUT':str(output)},check=True)
        cases=[]
        for path in sorted(output.glob('*.mp4')):
            run(['ffmpeg','-v','error','-xerror','-err_detect','explode','-i',path,'-map','0','-f','null','-'])
            probe=json.loads(subprocess.check_output(['ffprobe','-v','error','-show_streams','-show_packets','-show_data_hash','sha256','-of','json',str(path)]))
            assert probe['packets'] and probe['streams']
            assert all(s['codec_tag_string'] not in ['encv','enca'] for s in probe['streams'])
            cases.append({'name':path.name,'sha256':hashlib.sha256(path.read_bytes()).hexdigest(),'packets':len(probe['packets']),'codecs':[s['codec_name'] for s in probe['streams']],'packetEvidenceSha256':hashlib.sha256(json.dumps(probe,sort_keys=True).encode()).hexdigest(),'decoded':True})
        (output/'evidence.json').write_text(json.dumps({'generator':manifest['generator'],'ffmpeg':subprocess.check_output(['ffmpeg','-version'],text=True).splitlines()[0],'cases':cases},indent=2)+'\n')
        print(f'Independently parsed and decoded {len(cases)} clear outputs')
if __name__=='__main__': main()
