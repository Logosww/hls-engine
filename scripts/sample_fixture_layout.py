"""Synthetic ISO framing around independently OpenSSL-encrypted original samples.

Used only to exercise NAL widths and multi-run layout that packagers normalize.
The production Rust reader/cryptor is never used to generate expected values.
"""
import struct, subprocess

def box(k,b): return struct.pack('>I4s',len(b)+8,k)+b

def boxes(b):
    pos=0
    while pos<len(b):
        n,k=struct.unpack_from('>I4s',b,pos)
        assert n>=8 and pos+n<=len(b)
        yield pos,k,b[pos+8:pos+n]
        pos+=n
    assert pos==len(b)

def init(data,kid):
    out=b''
    for _,kind,payload in boxes(data):
        if kind in [b'moov',b'trak',b'mdia',b'minf',b'stbl']:
            payload=init(payload,kid)
        if kind==b'stsd':
            entries=b''
            for _,codec,entry in boxes(payload[8:]):
                assert codec in [b'avc1',b'avc3',b'mp4a']
                tenc=bytes([0,0,0,0,0,0,1,8])+bytes.fromhex(kid)
                sinf=box(b'frma',codec)+box(b'schm',bytes(4)+b'cenc'+bytes.fromhex('00010000'))+box(b'schi',box(b'tenc',tenc))
                entries+=box(b'enca' if codec==b'mp4a' else b'encv',entry+box(b'sinf',sinf))
            payload=payload[:8]+entries
        out+=box(kind,payload)
    return out

def segment(data,key):
    modified=bytearray(data); additions={}; counter=0
    for moof_pos,kind,payload in boxes(data):
        if kind!=b'moof': continue
        for traf_pos,k,traf in boxes(payload):
            if k!=b'traf': continue
            items=list(boxes(traf)); tfhd=next(b for _,t,b in items if t==b'tfhd')
            flags=int.from_bytes(tfhd[:4],'big')&0xffffff;assert flags&0x20000 and not flags&1
            p=8
            if flags&2:p+=4
            if flags&8:p+=4
            default_size=int.from_bytes(tfhd[p:p+4],'big') if flags&16 else 0
            records=[];cursor=None
            for _,t,b in items:
                if t!=b'trun':continue
                f=int.from_bytes(b[:4],'big')&0xffffff;n=int.from_bytes(b[4:8],'big');p=8
                if f&1:cursor=moof_pos+int.from_bytes(b[p:p+4],'big',signed=True);p+=4
                assert cursor is not None
                if f&4:p+=4
                for _ in range(n):
                    if f&0x100:p+=4
                    size=int.from_bytes(b[p:p+4],'big') if f&0x200 else default_size
                    if f&0x200:p+=4
                    if f&0x400:p+=4
                    if f&0x800:p+=4
                    counter+=1;iv=(1000+counter).to_bytes(8,'big');records.append(iv)
                    source=data[cursor:cursor+size];assert len(source)==size
                    encrypted=subprocess.check_output(['openssl','enc','-aes-128-ctr','-K',key,'-iv',(iv+bytes(8)).hex(),'-nopad'],input=source)
                    modified[cursor:cursor+size]=encrypted;cursor+=size
                assert p==len(b)
            additions[(moof_pos,traf_pos)]=box(b'senc',bytes(4)+len(records).to_bytes(4,'big')+b''.join(records))
    out=b''
    for moof_pos,kind,payload in boxes(modified):
        payload=bytes(payload)
        if kind==b'moof':
            extra=sum(len(v) for (m,_),v in additions.items() if m==moof_pos);parts=b''
            for traf_pos,k,traf in boxes(payload):
                if k==b'traf':
                    content=b''
                    for _,t,b in boxes(traf):
                        if t==b'trun' and int.from_bytes(b[:4],'big')&1:
                            offset=int.from_bytes(b[8:12],'big',signed=True)+extra;b=b[:8]+offset.to_bytes(4,'big',signed=True)+b[12:]
                        content+=box(t,b)
                    traf=content+additions[(moof_pos,traf_pos)]
                parts+=box(k,traf)
            payload=parts
        out+=box(kind,payload)
    return out

def generate(root,dest,key,kid):
    for width in [1,2]:
        source=root/'tests/fixtures/media'/f'fmp4_avc_nal{width}_multirun'
        for scheme in ['clear','cenc']:
            out=dest/f'fmp4_nal{width}_{scheme}';out.mkdir(exist_ok=True)
            data=(source/'init.fmp4').read_bytes();(out/'init.mp4').write_bytes(init(data,kid) if scheme=='cenc' else data)
            text='#EXTM3U\n#EXT-X-TARGETDURATION:3\n#EXT-X-MAP:URI="init.mp4"\n'
            if scheme=='cenc':text+='#EXT-X-KEY:METHOD=SAMPLE-AES-CTR,URI="key"\n'
            for i,path in enumerate(sorted(source.glob('seg*.m4s')),1):
                (out/f'seg{i}.m4s').write_bytes(segment(path.read_bytes(),key) if scheme=='cenc' else path.read_bytes())
                text+=f'#EXTINF:2,\nseg{i}.m4s\n'
            (out/'input.m3u8').write_text(text+'#EXT-X-ENDLIST\n')
