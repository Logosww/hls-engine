#!/usr/bin/env python3
"""Probe IINA's installed libmpv engine; do not claim IINA GUI rendering."""
import ctypes as c
import json
import os
from pathlib import Path
import sys
import time

libpath = os.environ.get('HLS_IINA_LIBRARY', '/Applications/IINA.app/Contents/Frameworks/libmpv.2.dylib')
try:
    lib = c.CDLL(libpath)
except OSError as error:
    print(json.dumps({'status': 'UNAVAILABLE', 'library': libpath, 'reason': str(error), 'results': []}, indent=2))
    sys.exit(0)
lib.mpv_create.restype = c.c_void_p
lib.mpv_set_option_string.argtypes = [c.c_void_p, c.c_char_p, c.c_char_p]
lib.mpv_initialize.argtypes = [c.c_void_p]
lib.mpv_command.argtypes = [c.c_void_p, c.POINTER(c.c_char_p)]
lib.mpv_get_property_string.argtypes = [c.c_void_p, c.c_char_p]
lib.mpv_get_property_string.restype = c.c_void_p
lib.mpv_free.argtypes = [c.c_void_p]
lib.mpv_terminate_destroy.argtypes = [c.c_void_p]
rows = []
for path in sys.argv[1:]:
    handle = lib.mpv_create()
    for k, v in [('vo','null'),('ao','null'),('pause','yes'),('terminal','no'),('config','no')]:
        assert lib.mpv_set_option_string(handle,k.encode(),v.encode()) >= 0
    assert lib.mpv_initialize(handle) >= 0
    def prop(key):
        p = lib.mpv_get_property_string(handle,key.encode())
        if not p: return None
        value = c.string_at(p).decode();lib.mpv_free(p);return value
    def command(*args):
        argv=(c.c_char_p*(len(args)+1))(*(a.encode() for a in args), None)
        return lib.mpv_command(handle,argv)
    command('loadfile',str(Path(path).resolve()))
    deadline=time.monotonic()+10
    while int(prop('track-list/count') or 0) == 0 and time.monotonic()<deadline: time.sleep(.05)
    tracks=[]
    for i in range(int(prop('track-list/count') or 0)):
        tracks.append({k:prop(f'track-list/{i}/{k}') for k in ['id','type','lang','title','codec','selected']})
    switches=[]
    for track in tracks:
        if track['type']=='audio':
            command('set','aid',track['id']);command('seek','0.75','absolute+exact')
            time.sleep(.1)
            switches.append({'requested':track['id'],'selected':prop('aid')})
    rows.append({'file':path,'engine':prop('mpv-version'),'tracks':tracks,'audioSwitches':switches,
                 'subtitleTracks':sum(t['type']=='sub' for t in tracks),'wvttSupported':any(t['type']=='sub' and t['codec'] in ('webvtt','wvtt') for t in tracks)})
    lib.mpv_terminate_destroy(handle)
print(json.dumps({'status':'OBSERVED','library':libpath,'results':rows},indent=2))
