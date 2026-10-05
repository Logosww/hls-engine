#!/usr/bin/env python3
"""Execute the actual SDK WASM host bridge in isolated headless Chrome."""
from functools import partial
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import tempfile
import threading
ROOT=Path(__file__).resolve().parents[1]

def verify():
    done=threading.Event()
    results=[]
    class Handler(SimpleHTTPRequestHandler):
        def log_message(self,*_): pass
        def do_POST(self):
            length=int(self.headers.get('Content-Length','0'))
            if self.path!='/result' or not 0<length<131072:
                self.send_error(400);return
            results.append(json.loads(self.rfile.read(length)))
            self.send_response(204);self.end_headers();done.set()
    chrome=os.environ.get('HLS_TEST_CHROME') or shutil.which('google-chrome') or shutil.which('chromium') or '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome'
    page=ROOT/'target/sdk-compat/browser.html'
    page.write_text('''<!doctype html><script type="module">
import {verify} from '/tests/support/sdk/browser.mjs';
try {const result=await verify();await fetch('/result',{method:'POST',body:JSON.stringify(result)});}
catch(e){await fetch('/result',{method:'POST',body:JSON.stringify({status:'FAIL',error:String(e),stack:e.stack})});}
</script>''')
    with ThreadingHTTPServer(('127.0.0.1',0),partial(Handler,directory=str(ROOT))) as server:
        thread=threading.Thread(target=server.serve_forever,daemon=True);thread.start()
        try:
            with tempfile.TemporaryDirectory(prefix='sdk-timeline-chrome-') as profile, tempfile.TemporaryFile(mode='w+') as log:
                process=subprocess.Popen([chrome,'--headless','--disable-gpu','--no-first-run','--disable-background-networking','--disable-extensions','--user-data-dir='+profile,f'http://127.0.0.1:{server.server_port}/target/sdk-compat/browser.html'],stdout=log,stderr=log,start_new_session=True)
                try:
                    completed=done.wait(120)
                finally:
                    # Reap the isolated browser process group, including profile writers.
                    for sig in (signal.SIGTERM, signal.SIGKILL):
                        try: os.killpg(process.pid, sig)
                        except (ProcessLookupError, PermissionError):
                            if process.poll() is None: process.send_signal(sig)
                        try: process.wait(timeout=5)
                        except subprocess.TimeoutExpired: pass
                log.seek(0)
                assert completed,log.read()[-4000:]
                assert len(results)==1 and results[0]['status']=='PASS',results
                (ROOT/'target/sdk-compat/browser-timeline.json').write_text(json.dumps(results[0],indent=2)+'\n')
                return results[0]
        finally:
            server.shutdown();thread.join()
if __name__=='__main__': print(json.dumps(verify()))
