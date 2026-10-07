/** Exercise the mixed five-track fragment through MSE, including append/decode. */
export async function probeDirectMse(video, data) {
  const results = [];
  for (const codecs of ['avc1.640028,mp4a.40.2,wvtt', 'avc1.640028,mp4a.40.2']) {
    const type = `video/mp4; codecs="${codecs}"`;
    const row = {type, advertised: typeof MediaSource !== 'undefined' && MediaSource.isTypeSupported(type)};
    let url;
    try {
      const media = new MediaSource(); url = URL.createObjectURL(media);
      const opened = new Promise((resolve, reject) => {
        const timer = setTimeout(()=>reject(Error('sourceopen timeout')),5000);
        media.addEventListener('sourceopen',()=>{clearTimeout(timer);resolve();},{once:true});
      });
      video.src = url; video.load(); video.play().catch(()=>{}); await opened;
      const buffer = media.addSourceBuffer(type); row.sourceBufferCreated = true;
      await new Promise((resolve,reject)=>{
        const timer=setTimeout(()=>reject(Error('append timeout')),5000);
        buffer.addEventListener('updateend',()=>{clearTimeout(timer);resolve();},{once:true});
        buffer.addEventListener('error',()=>{clearTimeout(timer);reject(Error('append decode error'));},{once:true});
        buffer.appendBuffer(data);
      });
      row.appended = true; media.endOfStream();
      await video.play(); await new Promise(r=>setTimeout(r,250)); video.pause();
      row.videoDecoded = video.videoWidth > 0 && video.currentTime > 0;
      row.audioTracks = video.audioTracks?.length ?? null;
      row.textTracks = video.textTracks.length;
      row.fullMultitrack = row.videoDecoded && row.audioTracks === 2 && row.textTracks === 2;
    } catch(error) { row.error = String(error); row.fullMultitrack = false; }
    finally { video.removeAttribute('src');video.load();if(url)URL.revokeObjectURL(url); }
    results.push(row);
  }
  return results;
}
