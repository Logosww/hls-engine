// Optional acceptance dependency: npm install playwright, or set HLS_PLAYWRIGHT
// to an existing playwright package directory. No browsers are downloaded.
import {createRequire} from 'node:module';
import {resolve} from 'node:path';
const require = createRequire(import.meta.url);
const {chromium} = require(process.env.HLS_PLAYWRIGHT || 'playwright');
const browser = await chromium.launch({executablePath: process.env.HLS_TEST_CHROME || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless:true,
  args:['--autoplay-policy=no-user-gesture-required']});
try {
  const page = await browser.newPage({viewport:{width:640,height:360},deviceScaleFactor:1});
  const cdp = await page.context().newCDPSession(page);
  await page.exposeFunction('capture', async name => {
    const path = resolve('target/multitrack',name+'.png');
    await page.screenshot({path});
    const tree=await cdp.send('DOM.getDocument',{depth:-1,pierce:true});
    const nodes=[];
    const walk=n=>{if(n.nodeType===3 && /^(Settings [0-4]|Hello|Overlap|こんにちは)$/.test(n.nodeValue))nodes.push(n);for(const c of [...(n.children||[]),...(n.shadowRoots||[])])walk(c);};walk(tree.root);
    const text=[];
    for(const n of nodes) {
      const {model}=await cdp.send('DOM.getBoxModel',{nodeId:n.nodeId});
      const q=model.content;
      text.push({text:n.nodeValue,bounds:[Math.min(q[0],q[2],q[4],q[6]),Math.min(q[1],q[3],q[5],q[7]),Math.max(q[0],q[2],q[4],q[6])-Math.min(q[0],q[2],q[4],q[6]),Math.max(q[1],q[3],q[5],q[7])-Math.min(q[1],q[3],q[5],q[7])]});
    }
    return {path,text};
  });
  await page.goto(process.argv[2]);
  await page.waitForFunction(()=>window.probeDone,{timeout:90000});
} finally { await browser.close(); }
