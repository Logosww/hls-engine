// macOS WebKit direct-file acceptance probe. Run after verify_multitrack.py.
import AppKit
import WebKit

final class Probe: NSObject, WKScriptMessageHandler {
    let root: URL
    var view: WKWebView!
    var window: NSWindow!
    init(root: URL) { self.root = root; super.init() }
    func start() throws {
        let configuration = WKWebViewConfiguration()
        configuration.userContentController.add(self, name: "result")
        configuration.mediaTypesRequiringUserActionForPlayback = []
        view = WKWebView(frame: NSRect(x:0,y:0,width:640,height:360), configuration: configuration)
        window = NSWindow(contentRect: view.frame, styleMask: [.titled], backing: .buffered, defer: false)
        window.title = "hls-transmux playback verification"
        window.contentView = view
        window.makeKeyAndOrderFront(nil)
        let media = try Data(contentsOf:root.appendingPathComponent("multitrack-fragmented.mp4")).base64EncodedString()
        let mse = try String(contentsOfFile:"scripts/multitrack_mse.mjs",encoding:.utf8).replacingOccurrences(of:"export async",with:"async")
        let html = """
        <!doctype html><meta charset="utf-8"><style>body{margin:0;background:black}video{width:640px;height:360px}</style><video muted controls></video><script>
        \(mse)
        const v=document.querySelector('video');
        const capture=name=>new Promise(resolve=>{window.captured=resolve;window.webkit.messageHandlers.result.postMessage({action:'snapshot',name});});
        const delay=ms=>new Promise(r=>setTimeout(r,ms));
        const event=(name)=>new Promise((res,rej)=>{const timer=setTimeout(()=>rej(Error('timeout '+name)),8000);v.addEventListener(name,()=>{clearTimeout(timer);res()},{once:true});v.addEventListener('error',()=>{clearTimeout(timer);rej(Error('media '+v.error.code))},{once:true});});
        (async()=>{const results=[];
        for(const file of ['multitrack-fragmented.mp4','multitrack-classic.mp4']){
            const loaded=event('loadeddata');v.src=file;await loaded;
            const row={file,videoDecoded:v.videoWidth>0,audio:[],subtitles:[]};
            for(const t of [...v.audioTracks]){for(const x of [...v.audioTracks])x.enabled=x===t;await v.play();await delay(150);v.pause();row.audio.push({id:t.id,language:t.language,enabled:t.enabled,playbackTime:v.currentTime});}
            for(const t of [...v.textTracks]){
                for(const x of [...v.textTracks])x.mode=x===t?'showing':'disabled';
                const rendered=[];
                for(const [name,time] of [['overlap',.75],...Array.from({length:5},(_,i)=>['settings-'+i,2.1+i*.6])]){
                    if(v.currentTime!==time){const seek=event('seeked');v.currentTime=time;await seek;}await delay(200);
                    const cues=[...(t.activeCues||[])].map(c=>({text:c.text,align:c.align,position:c.position,positionAlign:c.positionAlign,line:c.line,lineAlign:c.lineAlign,vertical:c.vertical,size:c.size}));
                    if(!cues.length)throw Error('missing '+name);
                    if(name==='overlap'&&cues.length!==2)throw Error('missing overlap');
                    const screenshot='webkit-'+file+'-'+t.language+'-'+name;
                    await capture(screenshot);rendered.push({name,cues,screenshot:screenshot+'.png'});
                }
                row.subtitles.push({language:t.language,rendered});
            }
            results.push(row);
        }
        const directMse=await probeDirectMse(v,Uint8Array.from(atob('\(media)'),c=>c.charCodeAt(0)));
        window.webkit.messageHandlers.result.postMessage({status:'PASS',runtime:navigator.userAgent,directMse,results});
        })().catch(e=>window.webkit.messageHandlers.result.postMessage({status:'ERROR',error:String(e)}));
        </script>
        """
        let file = root.appendingPathComponent("webkit-probe.html")
        try html.write(to:file,atomically:true,encoding:.utf8)
        if CommandLine.arguments.count > 2 {
            view.load(URLRequest(url:URL(string:CommandLine.arguments[2])!))
        } else { view.loadFileURL(file,allowingReadAccessTo:root) }
        DispatchQueue.main.asyncAfter(deadline:.now()+90) {
            print("{\"status\":\"TIMEOUT\"}")
            NSApplication.shared.terminate(nil)
        }
    }
    func userContentController(_ userContentController: WKUserContentController, didReceive message: WKScriptMessage) {
        if let body=message.body as? [String:Any], body["action"] as? String == "snapshot", let name=body["name"] as? String {
            view.takeSnapshot(with:nil) { image, error in
                if let image, let data=image.tiffRepresentation, let bitmap=NSBitmapImageRep(data:data), let png=bitmap.representation(using:.png,properties:[:]) {
                    try? png.write(to:self.root.appendingPathComponent(name+".png"))
                }
                self.view.evaluateJavaScript("window.captured()")
            }
        } else {
            if let data=try? JSONSerialization.data(withJSONObject:message.body,options:[.prettyPrinted,.sortedKeys]),let text=String(data:data,encoding:.utf8){print(text)}
            NSApplication.shared.terminate(nil)
        }
    }
}
let app=NSApplication.shared
app.setActivationPolicy(.accessory)
let probe=Probe(root:URL(fileURLWithPath:CommandLine.arguments[1]).standardizedFileURL)
try probe.start()
app.run()
