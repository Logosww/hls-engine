#!/usr/bin/env python3
"""Compare continuous clear outputs against retained independent source fixtures."""
import hashlib, json, os, subprocess
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
OUT=ROOT/'target/continuous-output'

def command(*args):
    return subprocess.check_output([str(a) for a in args],cwd=ROOT)

def decode(path):
    probe=json.loads(command('ffprobe','-v','error','-show_streams','-show_packets','-of','json',path))
    result={}
    for stream in probe['streams']:
        index=stream['index'];kind=stream['codec_type']
        args=['ffmpeg','-v','error','-xerror','-err_detect','explode','-i',path,'-map',f'0:{index}']
        if kind=='audio':
            # Decode exactly the declared sample duration; fMP4 decoders can
            # otherwise expose encoder padding past a short final AAC packet.
            from fractions import Fraction
            duration=sum(int(p.get('duration',0)) for p in probe['packets'] if p['stream_index']==index)
            samples=int(duration*Fraction(stream['time_base'])*int(stream['sample_rate']))
            args+=['-af',f'atrim=end_sample={samples}']
        else: args+=['-fps_mode','passthrough']
        text=command(*args,'-f','framemd5','-').decode()
        result[kind]=[line.split(',')[-1].strip() for line in text.splitlines() if line and not line.startswith('#')]
    return result

def main():
    OUT.mkdir(parents=True,exist_ok=True)
    result=subprocess.check_output(['cargo','run','--offline','--features','serde','--example','continuous_verify'],cwd=ROOT,env={**os.environ,'HLS_CONTINUOUS_OUTPUT':str(OUT)})
    reports=json.loads(result)
    (OUT/'reports.json').write_text(json.dumps(reports,indent=2)+'\n')
    evidence=[]
    for row in reports:
        name,mode=row['name'].rsplit('-',1)
        clear=name.replace('_cenc','_clear').replace('_cbcs','_clear').replace('_sample','_clear')
        directory=ROOT/'tests/fixtures/sample_crypto'/clear
        reference=OUT/(clear+'.reference')
        init=directory/'init.mp4'
        files=([init] if init.exists() else [])+sorted(directory.glob('seg*'))
        reference.write_bytes(b''.join(p.read_bytes() for p in files))
        output=OUT/(row['name']+'.mp4')
        frames=decode(output)
        assert frames==decode(reference),f'decoded payload mismatch: {name}/{mode}'
        probe=json.loads(command('ffprobe','-v','error','-show_streams','-show_packets','-show_data_hash','sha256','-of','json',output))
        assert probe['packets'] and all(s['codec_tag_string'] not in ['encv','enca'] for s in probe['streams'])
        clear_row=next(r for r in reports if r['name']==clear+'-'+mode)
        assert row['hash']==clear_row['hash'],f'normalized clear sample/timing mismatch: {name}/{mode}'
        evidence.append({'name':row['name'],'decodedFrames':sum(map(len,frames.values())),'packets':len(probe['packets']),'normalizedOutputSha256':row['hash'],'clearReferenceEqual':True,'packetEvidenceSha256':hashlib.sha256(json.dumps(probe,sort_keys=True).encode()).hexdigest()})
    subprocess.run(['cargo','test','--offline','--test','continuous_session','aes128_rotations'],cwd=ROOT,env={**os.environ,'HLS_CONTINUOUS_OUTPUT':str(OUT)},check=True)
    for output in sorted(OUT.glob('aes-*.mp4')):
        name,mode=output.stem[4:].rsplit('-',1)
        folder=ROOT/'tests/fixtures/media'/name
        init=folder/'init.fmp4'
        reference=OUT/(name+'.reference')
        files=([init] if init.exists() else [])+[folder/f'seg{i}.{ "m4s" if init.exists() else "ts" }' for i in range(3)]
        reference.write_bytes(b''.join(p.read_bytes() for p in files))
        frames=decode(output)
        assert frames==decode(reference),f'AES decode mismatch: {name}/{mode}'
        evidence.append({'name':output.stem,'decodedFrames':sum(map(len,frames.values())),'clearReferenceEqual':True,'sha256':hashlib.sha256(output.read_bytes()).hexdigest()})
    native_args=['cargo','test','--offline','--test','continuous_session','native_continuous_finalize_backends']
    if os.environ.get('HLS_CONTINUOUS_FFMPEG')=='1': native_args+=['--all-features']
    subprocess.run(native_args,cwd=ROOT,env={**os.environ,'HLS_CONTINUOUS_OUTPUT':str(OUT)},check=True)
    for output in sorted(OUT.glob('native-*.mp4')):
        name,backend=output.stem[7:].rsplit('-',1)
        clear=name.replace('_cenc','_clear').replace('_cbcs','_clear').replace('_sample','_clear')
        frames=decode(output)
        assert frames==decode(OUT/(clear+'.reference')),f'native decode mismatch: {name}/{backend}'
        evidence.append({'name':output.stem,'decodedFrames':sum(map(len,frames.values())),'clearReferenceEqual':True,'sha256':hashlib.sha256(output.read_bytes()).hexdigest()})
    record={'status':'PASS','ffmpeg':command('ffmpeg','-version').decode().splitlines()[0],'cases':evidence}
    (OUT/'evidence.json').write_text(json.dumps(record,indent=2)+'\n')
    print(f'Continuous: {len(evidence)} outputs independently parsed, decoded and compared with clear source fixtures')
if __name__=='__main__': main()
