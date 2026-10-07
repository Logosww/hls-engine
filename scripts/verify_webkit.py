#!/usr/bin/env python3
"""Run the WebKit probe on HTTP (file origins do not exercise MSE reliably)."""
from functools import partial
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import json
import subprocess
import threading

ROOT=Path(__file__).resolve().parents[1]
OUT=ROOT/'target/multitrack'
binary=ROOT/'target/webkit-probe'
subprocess.run(['swiftc','-module-cache-path',str(ROOT/'target/swift-cache'),
                str(ROOT/'scripts/probe_webkit.swift'),'-o',str(binary)],check=True)
class Handler(SimpleHTTPRequestHandler):
    def log_message(self,*_): pass
with ThreadingHTTPServer(('127.0.0.1',0),partial(Handler,directory=str(OUT))) as server:
    worker=threading.Thread(target=server.serve_forever,daemon=True);worker.start()
    try:
        with (OUT/'webkit.log').open('w') as log:
            result=subprocess.check_output([str(binary),str(OUT),f'http://127.0.0.1:{server.server_port}/webkit-probe.html'],cwd=ROOT,stderr=log,timeout=95)
        evidence=json.loads(result)
        (OUT/'webkit-evidence.json').write_text(json.dumps(evidence,indent=2,ensure_ascii=False)+'\n')
        assert evidence['status']=='PASS',evidence
        assert all(len(r['audio'])==2 and len(r['subtitles'])==2 for r in evidence['results'])
        assert all(len(t['rendered'])==6 for r in evidence['results'] for t in r['subtitles'])
        expected=[{'align':'end','position':90,'positionAlign':'line-right','line':20,'lineAlign':'center','size':50},
                  {'align':'start','position':20,'line':2,'vertical':'rl'},
                  {'align':'left','position':50,'positionAlign':'center','line':-1,'lineAlign':'end','vertical':'lr'},
                  {'align':'right','position':80,'positionAlign':'auto','line':'auto'},
                  {'align':'center','position':10,'positionAlign':'line-left','line':1,'lineAlign':'start'}]
        for row in evidence['results']:
            for track in row['subtitles']:
                for index, sample in enumerate(track['rendered'][1:]):
                    cue=sample['cues'][0]
                    assert all(cue.get(k)==v for k,v in expected[index].items()),(row['file'],index,cue)
                assert all((OUT/s['screenshot']).is_file() for s in track['rendered'])
        assert all('timeout' not in r.get('error','') for r in evidence['directMse']),evidence['directMse']
        print(json.dumps({'status':'PASS','directMse':evidence['directMse']}))
    finally:
        server.shutdown();worker.join()
