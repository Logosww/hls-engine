import {probeDirectMse} from '/scripts/multitrack_mse.mjs';
import {createWvttDisplayer} from '/scripts/shaka_wvtt_adapter.mjs';
const assert = (v, message) => { if (!v) throw new Error(message); };
const bytes = async path => new Uint8Array(await (await fetch('/target/multitrack/'+path)).arrayBuffer());
const event = (target, name) => new Promise((resolve,reject) => {
  const timer=setTimeout(()=>reject(new Error('timeout '+name)),10000);
  target.addEventListener(name,()=>{clearTimeout(timer);resolve();},{once:true});
  target.addEventListener('error',()=>{clearTimeout(timer);reject(new Error('media error '+target.error?.code));},{once:true});
});
try {
  const cases=await (await fetch('/target/multitrack/player-cases.json')).json();
  const video=document.querySelector('video'), container=document.querySelector('#container');
  const player=new shaka.Player(); await player.attach(video); player.setVideoContainer(container);
  const results=[], adapterGeometry=[];
  for(const c of cases) {
    const blob=URL.createObjectURL(new Blob([await bytes(c.file)],{type:'video/mp4'}));
    const loaded=event(video,'loadeddata');video.src=blob;await loaded;
    const sought=event(video,'seeked');video.currentTime=0.75;await sought;
    const row={file:c.file,blobVideoDecoded:video.videoWidth>0,nativeAudioTracks:video.audioTracks?.length??null,nativeTextTracks:video.textTracks.length,tracks:[]};
    for(const t of c.tracks) {
      const parser=new shaka.text.Mp4VttParser();parser.parseInit(await bytes(t.init));
      const cues=parser.parseMedia(await bytes(t.media),{periodStart:0,segmentStart:0,segmentEnd:10,vttOffset:0});
      const active=cues.filter(c=>c.startTime<=0.75&&c.endTime>0.75);
      assert(active.length===2,'missing overlapping cues');
      const first=active.find(c=>c.payload!=='Overlap');
      assert(first.textAlign==='start'&&first.position===10,'settings lost');
      if(video.currentTime!==0.75){const sought=event(video,'seeked');video.currentTime=0.75;await sought;}
      const display=createWvttDisplayer(player,shaka);display.configure(player.getConfiguration().textDisplay);
      display.setTextVisibility(true);display.append(cues);
      await new Promise(r=>setTimeout(r,150));
      const text=container.querySelector('.shaka-text-container').textContent;
      assert(text.includes(first.payload)&&text.includes('Overlap'),'subtitle DOM missing '+text);
      const overlapScreenshot=await window.capture(`${c.file}-${t.language}-overlap`);
      row.tracks.push({overlapScreenshot,id:t.track,language:t.language,parsed:cues.length,active:active.length,rendered:text,position:first.position,textAlign:first.textAlign});
      const settings=[];
      const expected=[
        {textAlign:'end',position:90,positionAlign:'line-right',line:20,lineAlign:'center',size:50},
        {textAlign:'start',position:20,line:2,writingMode:'vertical-rl'},
        {textAlign:'left',position:50,positionAlign:'center',line:-1,lineAlign:'end',writingMode:'vertical-lr'},
        {textAlign:'right',position:80,positionAlign:'auto',line:null},
        {textAlign:'center',position:10,positionAlign:'line-left',line:1,lineAlign:'start'},
      ];
      for(let index=0;index<expected.length;index++) {
        const cue=cues.find(c=>c.payload===`Settings ${index}`);
        assert(cue,'missing settings cue '+index);
        for(const [key,value] of Object.entries(expected[index])) assert(cue[key]===value,`settings ${index} ${key}: ${cue[key]} != ${value}`);
        const sought=event(video,'seeked');video.currentTime=cue.startTime+0.1;await sought;
        await new Promise(r=>setTimeout(r,200));
        const nodes=[...container.querySelectorAll('.shaka-text-container *')];
        const visible=nodes.filter(n=>n.textContent===cue.payload&&n.getBoundingClientRect().width>0&&n.getBoundingClientRect().height>0);
        assert(visible.length,'settings DOM missing '+index);
        const styles=visible.map(n=>{const s=getComputedStyle(n);return {writingMode:s.writingMode,textAlign:s.textAlign,width:s.width,left:s.left,top:s.top};});
        if(expected[index].writingMode) assert(styles.some(s=>s.writingMode===expected[index].writingMode),'writing mode not rendered');
        const outer=visible[0].getBoundingClientRect(), bounds=container.getBoundingClientRect();
        const percentageLinePositionEqual=index===0 ? Math.abs((outer.top-bounds.top+outer.height/2)/bounds.height*100-20)<2 : null;
        if(index===0) assert(percentageLinePositionEqual,'percentage line center geometry mismatch');
        const screenshot=await window.capture(`${c.file}-${t.language}-settings-${index}`);
        settings.push({index,parsed:expected[index],styles,percentageLinePositionEqual,screenshot});
      }
      row.tracks.at(-1).settings=settings;
      await display.destroy();
      if(results.length===0 && row.tracks.length===1) {
        const source=cues.find(c=>c.payload==='Settings 0');
        const seek=event(video,'seeked');video.currentTime=source.startTime+.1;await seek;
        for(const writingMode of ['horizontal-tb','vertical-rl','vertical-lr']) {
          const cue=source.clone();cue.startTime=0;cue.endTime=10;cue.writingMode=writingMode;cue.line=40;cue.lineAlign='center';cue.position=60;cue.positionAlign='center';cue.textAlign='center';cue.size=50;
          const displayer=createWvttDisplayer(player,shaka);displayer.configure(player.getConfiguration().textDisplay);displayer.setTextVisibility(true);displayer.append([cue]);
          await new Promise(r=>setTimeout(r,150));
          const node=[...container.querySelectorAll('.shaka-text-container *')].find(n=>n.textContent===cue.payload && n.getBoundingClientRect().width>0);
          assert(node,'missing adapter probe cue '+writingMode);
          const outer=node.getBoundingClientRect();
          const bounds=container.getBoundingClientRect();
          const actual=writingMode==='horizontal-tb'?(outer.top-bounds.top+outer.height/2)/bounds.height:(outer.left-bounds.left+outer.width/2)/bounds.width;
          const expected=writingMode==='vertical-rl'?.6:.4;
          assert(Math.abs(actual-expected)<.005,'center axis mismatch '+writingMode+' '+actual);
          const screenshot=await window.capture('adapter-center-'+writingMode);
          adapterGeometry.push({writingMode,actual,expected,screenshot});await displayer.destroy();
        }
      }

    }
    video.removeAttribute('src');video.load();URL.revokeObjectURL(blob);
    await player.load('/target/multitrack/'+c.dash);
    const variants=player.getVariantTracks();
    assert(new Set(variants.map(t=>t.language)).size===2,'two audio languages missing');
    row.audioSwitches=[];
    for(const track of variants) {
      player.selectVariantTrack(track,true,0);
      await video.play();
      await new Promise(r=>setTimeout(r,350));
      const selected=player.getVariantTracks().find(t=>t.active);
      assert(selected.id===track.id,'audio switch not active');
      assert(video.currentTime>0&&!video.error,'playback did not advance');
      row.audioSwitches.push({id:track.id,language:track.language,active:true,time:video.currentTime});
    }
    video.pause();await player.unload();results.push(row);
  }
  await player.destroy();
  const directMse=await probeDirectMse(video,await bytes('multitrack-fragmented.mp4'));
  assert(directMse.every(r=>!r.fullMultitrack),'update capability evidence: direct MSE now supports full tracks');
  await fetch('/result',{method:'POST',body:JSON.stringify({status:'PASS',adapterGeometry,directMse,renderer:'Shaka 5.2.12 MP4 VTT parser + version-checked percentage-line display adapter',runtime:navigator.userAgent,shaka:shaka.Player.version,results,directMixedSubtitlePlayback:'not supported/claimed; extraction adapter only',audioSwitching:'verified via independent FFmpeg copy extraction to DASH and Shaka variant selection'})});
} catch(error) {
  await fetch('/result',{method:'POST',body:JSON.stringify({status:'FAIL',error:String(error),stack:error.stack})});
}

window.probeDone=true;
