"""Reproducible timeline integration cases for the real SDK host adapters."""
import copy
from pathlib import Path
ROOT = Path(__file__).resolve().parents[1]

def cases():
    results=[]
    def snapshot(name, encrypted=False):
        folder=f'tests/fixtures/{"crypto" if encrypted else "media"}/{name}'
        text=(ROOT/folder/'input.m3u8').read_text()
        base=f'https://sdk.test/{folder}/'
        files={base+p.name:str(p.relative_to(ROOT)) for p in (ROOT/folder).iterdir() if p.is_file()}
        if encrypted:
            extension='m4s' if name.startswith('fmp4') else 'ts'
            files[base+'clear.bin']=f'tests/fixtures/media/{name}/seg2.{extension}'
        return {'url':base+'input.m3u8','text':text},files
    def add(name,primary,files,selection=None,audio=None,mode='bytes',outputs=1):
        results.append({'name':name,'files':files,'outputs':outputs,'selection':{'range':None,'collapse':False,'split':False,**(selection or {})},'request':{'wireVersion':2,'primary':primary,'audio':audio,'mode':mode,'output':None,'operationId':name,'scope':'fixture','limits':{'resourceBytes':16777216,'waitingBytes':33554432,'resources':2,'keyRequests':2,'cachedKeys':16,'keyWaiters':16},'keyFormats':[{'format':'identity','versions':[1]}],'encryptedRanges':'complete-resources'}})
    request_range={'range':{'start':{'ticks':'200','timescale':1000},'end':{'ticks':'4200','timescale':1000}}}
    for encrypted in (False,True):
        p,files=snapshot('ts_avc_regular',encrypted)
        add(f'range-{encrypted}',p,files,request_range)
        a,af=snapshot('fmp4_aac_audio_only',encrypted)
        dual_range=copy.deepcopy(request_range);dual_range['range']['start']['ticks']='2200'
        add(f'dual-{encrypted}',p,files|af,dual_range,a)
    p,files=snapshot('ts_avc_video_only')
    reset=copy.deepcopy(p);reset['text']='#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\nseg0.ts\n#EXT-X-DISCONTINUITY\n#EXTINF:2,\nseg0.ts\n#EXT-X-ENDLIST\n'
    add('clock-reset',reset,files)
    gap=copy.deepcopy(p);gap['text']=gap['text'].replace('#EXTINF:2.000000,\nseg1.ts','#EXT-X-GAP\n#EXTINF:2.000000,\nseg1.ts')
    add('gap-collapse',gap,files,{'collapse':True})
    add('gap-split',gap,files,{'split':True},outputs=2)
    add('gap-preserve-stream',gap,files,mode='stream')
    h,hf=snapshot('ts_hevc_regular')
    p,pf=snapshot('ts_avc_regular');files=files|pf
    config=copy.deepcopy(reset);config['url']=p['url'];config['text']=config['text'].replace('#EXT-X-DISCONTINUITY\n#EXTINF:2,\nseg0.ts','#EXT-X-DISCONTINUITY\n#EXTINF:2,\n'+h['url'].replace('input.m3u8','seg0.ts'))
    add('config-split',config,files|hf,{'split':True},outputs=2)
    for name in ['fmp4_avc_cenc','fmp4_hevc_cbcs','fmp4_aac_cenc','ts_avc_sample','ts_aac_sample']:
        folder=ROOT/'tests/fixtures/sample_crypto'/name
        base=f'https://sdk.test/tests/fixtures/sample_crypto/{name}/'
        p={'url':base+'input.m3u8','text':(folder/'input.m3u8').read_text()}
        files={base+f.name:str(f.relative_to(ROOT)) for f in folder.iterdir() if f.is_file()}
        add('sample-'+name,p,files,request_range)
        if name=='fmp4_avc_cenc':
            audio=ROOT/'tests/fixtures/sample_crypto/fmp4_aac_cbcs';ab='https://sdk.test/tests/fixtures/sample_crypto/fmp4_aac_cbcs/'
            a={'url':ab+'input.m3u8','text':(audio/'input.m3u8').read_text()}
            add('sample-dual-schemes',p,files|{ab+f.name:str(f.relative_to(ROOT)) for f in audio.iterdir() if f.is_file()},request_range,a)
            rotated=copy.deepcopy(p)
            rotated['text']=rotated['text'].replace('#EXTINF:2.000,\nseg2.m4s','#EXT-X-KEY:METHOD=SAMPLE-AES-CTR,URI="key-rotation"\n#EXTINF:2.000,\nseg2.m4s')
            add('sample-range-key-redeclaration',rotated,files,request_range)
    return results


def multitrack_cases():
    result=[copy.deepcopy(next(c for c in cases() if c['name']=='sample-dual-schemes'))]
    result[0]['name']='multi-video-two-audio'
    for profile in ['clear','aes128','sample_aes']:
        c=copy.deepcopy(result[0]);c['name']='packed-'+profile;c['request']['audio']=None
        folder=ROOT/'tests/fixtures/packed_aac'/profile
        base='https://sdk.test/packed/'+profile+'/'
        c['request']['primary']={'url':base+'input.m3u8','text':(folder/'input.m3u8').read_text()}
        c['files']={base+f.name:str(f.relative_to(ROOT)) for f in folder.iterdir() if f.is_file()}
        result.append(c)
    mixed=copy.deepcopy(result[0]);mixed['name']='multi-packed-audio'
    mixed['request']['audio']=result[-1]['request']['primary'];mixed['files']|=result[-1]['files'];result.append(mixed)
    return result
