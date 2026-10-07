#!/usr/bin/env python3
"""Read actual libVLC track selection; isolated VLC distribution via HLS_VLC_ROOT."""
import ctypes as c
import json
import os
from pathlib import Path
import time
import sys
import subprocess

root = Path(os.environ.get('HLS_VLC_ROOT', '/Applications/VLC.app/Contents/MacOS'))
os.environ['VLC_PLUGIN_PATH'] = str(root/'plugins')
lib = c.CDLL(str(root/'lib/libvlc.dylib'))
def api(name, result, args):
    fn = getattr(lib, 'libvlc_'+name); fn.restype = result; fn.argtypes = args
    return fn
ptr = c.c_void_p
new = api('new', ptr, [c.c_int, c.POINTER(c.c_char_p)])
media = api('media_new_path', ptr, [ptr,c.c_char_p])
player = api('media_player_new_from_media',ptr,[ptr])
play = api('media_player_play',c.c_int,[ptr])
stop = api('media_player_stop',None,[ptr])
release = api('media_player_release',None,[ptr])
class Desc(c.Structure): pass
Desc._fields_ = [('id',c.c_int),('name',c.c_char_p),('next',c.POINTER(Desc))]
audio = api('audio_get_track_description',c.POINTER(Desc),[ptr])
subs = api('video_get_spu_description',c.POINTER(Desc),[ptr])
free = api('track_description_list_release',None,[c.POINTER(Desc)])
set_audio = api('audio_set_track',c.c_int,[ptr,c.c_int])
get_audio = api('audio_get_track',c.c_int,[ptr])
version = api('get_version',c.c_char_p,[])().decode()
options = [b'--ignore-config',b'--no-video-title-show',b'--vout=vmem',b'--aout=dummy',b'--quiet']
instance = new(len(options),(c.c_char_p*len(options))(*options))
assert instance
def tracks(fn,p):
    head = cursor = fn(p); result = []
    while cursor:
        item=cursor.contents
        if item.id >= 0: result.append({'id':item.id,'name':item.name.decode()})
        cursor=item.next
    if head: free(head)
    return result
locktype=c.CFUNCTYPE(ptr,ptr,c.POINTER(ptr))
unlocktype=c.CFUNCTYPE(None,ptr,ptr,c.POINTER(ptr))
displaytype=c.CFUNCTYPE(None,ptr,ptr)
buffer=c.create_string_buffer(640*360*4)
frames=[0]
@locktype
def lock(opaque,planes): planes[0]=c.addressof(buffer);return None
@unlocktype
def unlock(opaque,picture,planes): pass
@displaytype
def display(opaque,picture): frames[0]+=1
callbacks=api('video_set_callbacks',None,[ptr,locktype,unlocktype,displaytype,ptr])
format_video=api('video_set_format',None,[ptr,c.c_char_p,c.c_uint,c.c_uint,c.c_uint])
set_sub=api('video_set_spu',c.c_int,[ptr,c.c_int])
set_time=api('media_player_set_time',None,[ptr,c.c_longlong])
pause=api('media_player_set_pause',None,[ptr,c.c_int])
rows=[]
for path in sys.argv[1:]:
    m=media(instance,str(Path(path).resolve()).encode()); p=player(m)
    callbacks(p,lock,unlock,display,None);format_video(p,b'RV32',640,360,640*4)
    assert play(p)==0
    time.sleep(.8)
    row={'file':path,'audio':tracks(audio,p),'subtitles':tracks(subs,p),'audioSwitches':[]}
    for track in row['audio']:
        assert set_audio(p,track['id'])==0
        time.sleep(.1)
        row['audioSwitches'].append({'requested':track['id'],'selected':get_audio(p)})
    stop(p);release(p)
    row['rendered']=[]
    for track in row['subtitles']:
        p=player(m);callbacks(p,lock,unlock,display,None);format_video(p,b'RV32',640,360,640*4)
        assert play(p)==0
        deadline=time.monotonic()+3
        while not tracks(subs,p) and time.monotonic()<deadline: time.sleep(.01)
        assert set_sub(p,track['id'])==0
        for name,ms in [('overlap',500),*[(f'settings-{i}',2100+i*600) for i in range(5)]]:
            deadline=time.monotonic()+8
            clock=api('media_player_get_time',c.c_longlong,[ptr])
            while clock(p)<ms and time.monotonic()<deadline: time.sleep(.01)
            assert ms <= clock(p) < ms+500,('playback clock',path,name,clock(p))
            output=Path('target/multitrack')/f'vlc-{Path(path).stem}-{track["id"]}-{name}.png'
            pixels=buffer.raw
            subprocess.run(['ffmpeg','-v','error','-y','-f','rawvideo','-pixel_format','bgr0','-video_size','640x360','-i','pipe:0','-frames:v','1',str(output)],input=pixels,check=True)
            white=[(i%640,i//640) for i in range(640*70,640*360)
                   if min(pixels[i*4:i*4+3])>190 and max(pixels[i*4:i*4+3])-min(pixels[i*4:i*4+3])<20]
            bounds=None
            if white:
                xs,ys=zip(*white);bounds=[min(xs),min(ys),max(xs)-min(xs)+1,max(ys)-min(ys)+1]
            row['rendered'].append({'track':track['id'],'setting':name,'screenshot':str(output),'timeMs':clock(p),'captionPixelBounds':bounds})
        stop(p);release(p)
    rows.append(row)
    api('media_release',None,[ptr])(m)
api('release',None,[ptr])(instance)
print(json.dumps({'status':'OBSERVED','version':version,'library':str(root),'results':rows},indent=2))
