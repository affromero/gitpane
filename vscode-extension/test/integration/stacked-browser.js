'use strict';

const fs = require('node:fs');
const http = require('node:http');
const Module = require('node:module');
const os = require('node:os');
const path = require('node:path');
const { spawn } = require('node:child_process');

const extension = path.resolve(__dirname, '../..');
const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'gitpane-stacked-browser-'));
const browser = process.env.CHROME_EXECUTABLE || (process.platform === 'darwin'
  ? '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome' : 'google-chrome');
const assets = new Map();
const server = http.createServer((request, response) => {
  const asset = assets.get(request.url);
  if (!asset) { response.writeHead(404); response.end(); return; }
  response.setHeader('Content-Type', asset.type);
  response.end(asset.body);
});
const add = (route, body, type = 'text/html') => assets.set(route, { body, type });
for (const file of ['webview/styles.css', 'webview/view.js', 'graph/styles.css', 'graph/view.js', 'theme-webview.js']) {
  add(`/src/${file}`, fs.readFileSync(path.join(extension, 'src', file)), file.endsWith('.css') ? 'text/css' : 'text/javascript');
}

const scenario = String.raw`
const received = [];
const scriptErrors = [];
const ready = new Set();
let activeOwner = 'api', epoch = 1, retrying = false;
const owner = (id, name, branch, dirty, groups = []) => ({ id, name, branch, dirty, ahead: 0, behind: 0,
  stashCount: 0, worktreeCount: 0, worktrees: [], groups, changeCountLowerBound: groups.reduce((n,g)=>n+g.files.length,0),
  changesPage: { state: 'ready', page: 0, previous: false, next: false } });
const file = (id, name, status) => ({ id, name, path: name, status });
const group = (id, label, files) => ({ id, label, files });
const apiRepo = owner('api','api-service','main',true,[
  group('index','Staged',[file('api-staged','src/routes/users.ts','M')]),
  group('working','Unstaged',[file('api-working','src/services/auth.ts','M')]),
  group('untracked','Untracked',[file('api-new','tests/auth.test.ts','?')])]);
const linked = { ...owner('auth','api-auth','feature/oauth',true,[group('working','Unstaged',[file('auth-file','src/oauth.ts','M')])]), available:true };
apiRepo.worktrees=[linked];apiRepo.worktreeCount=1;
const webRepo = owner('web','web-app','feature/dashboard',true,[group('working','Unstaged',[file('web-file','src/dashboard.ts','M')])]);
const cleanRepo = owner('lib','shared-lib','main',false);
const repositories=[apiRepo,webRepo,cleanRepo];
const owners={api:apiRepo,auth:linked,web:webRepo,lib:cleanRepo};
const nativeTheme={name:'default',values:{'graph-commit-id':'rgb(229, 229, 16)','graph-merge-message':'rgb(130, 130, 130)',
 'graph-head-marker':'rgb(13, 188, 121)','graph-paren':'rgb(229, 229, 16)'},
 lanes:['rgb(205, 49, 49)','rgb(13, 188, 121)','rgb(229, 229, 16)','rgb(36, 114, 200)','rgb(188, 63, 188)','rgb(17, 168, 205)'],
 authors:['rgb(59, 142, 234)','rgb(35, 209, 139)','rgb(41, 184, 219)','rgb(214, 112, 214)','rgb(241, 76, 76)','rgb(245, 245, 67)','rgb(255, 165, 0)','rgb(180, 150, 255)']};
const win = id => document.getElementById(id).contentWindow;
const doc = id => document.getElementById(id).contentDocument;
function snapshot() {
 const selectedOwner=structuredClone(owners[activeOwner]);
 if(retrying){selectedOwner.groups=[];selectedOwner.changesPage.state='error';selectedOwner.changesPage.error='Temporary status read failed.';}
 return {type:'snapshot',repositories,selectedOwner,selectedOwnerId:activeOwner,selectedKind:activeOwner==='auth'?'worktree':'repo',
  selectionEpoch:epoch,separateChanges:true,expandWorktrees:true,errors:[],loading:false};
}
function graphSnapshot() {
 const prefix={api:'a',auth:'b',web:'c',lib:'d'}[activeOwner];
 const subjects=['Merge workspace updates','Keep worktree selection current','Open file changes in VS Code',
  'Refresh repository status','Show linked worktrees','Preserve graph lane colors','Update selected repository','Load commit details'];
 return {type:'graph',repoName:owners[activeOwner].name,loading:false,firstParent:false,
  filters:{branches:null,authors:null,refs:{local:true,remote:true,tags:true,stashes:true}},rows:Array.from({length:40},(_,i)=>({
   oid:prefix.repeat(32)+i.toString(16).padStart(8,'0'),short_id:prefix.repeat(6)+(i%10),message:subjects[i%subjects.length],
   author:'Andres Romero',time:1793380000-i*3600,is_merge:i===0,commit_col:i===1?1:0,
   lanes:i===0?['commit','cross_horizontal','merge_left']:i===1?['straight','commit']:['commit'],
   horizontal_spans:i===0?[[0,2,1]]:[],parent_oids:[],labels:i===0?[{name:owners[activeOwner].branch,is_head:true}]:[],diff_stat:null}))};
}
function update() {
 if(ready.has('repos'))win('repos').postMessage(snapshot(),'*');
 if(ready.has('changes'))win('changes').postMessage(snapshot(),'*');
 if(ready.has('graph')){win('graph').postMessage({type:'theme',theme:nativeTheme},'*');win('graph').postMessage(graphSnapshot(),'*');}
 document.getElementById('changes-heading').textContent='Changes: '+owners[activeOwner].name;
 document.getElementById('graph-heading').textContent='Git Graph: '+owners[activeOwner].name;
}
window.addEventListener('message',event=>{
 if(!event.data?.surface)return;
 if(event.data.scriptError){scriptErrors.push(event.data.scriptError);return;}
 const {surface,message}=event.data;received.push({surface,...message});
 if(message.type==='ready'){ready.add(surface);if(ready.size===3)update();}
 if(message.type==='selectOwner'){activeOwner=message.id;epoch++;retrying=false;update();}
 if(message.type==='loadChanges'&&retrying){retrying=false;win('changes').postMessage(snapshot(),'*');}
 if(surface==='graph'&&message.type==='selectCommit')win('graph').postMessage({type:'commitFiles',oid:message.oid,files:[{id:'commit-file',path:'src/index.ts',status:'M'}]},'*');
});
window.addEventListener('DOMContentLoaded',()=>{(async()=>{
 let checks=0;
 const check=(label,value)=>{if(!value)throw new Error(label);checks++;};
 const tick=()=>new Promise(resolve=>setTimeout(resolve,35));
 const wait=async condition=>{for(let n=0;n<80&&!condition();n++){if(scriptErrors.length)throw new Error(scriptErrors.join('\n'));await tick();}check('View initialization completed',condition());};
 const row=id=>doc('repos').querySelector('[data-key="repo:'+id+'"] > .row');
 try{
  await wait(()=>ready.size===3&&doc('graph').querySelectorAll('.commit').length===40);await tick();
  check('Changes are a separate section',doc('changes').body.dataset.view==='changes'&&doc('changes').querySelector('.file-row'));
  check('Repo list stays free of duplicate file rows',!doc('repos').querySelector('.file-row'));
  check('Repo search toolbar is reserved for Changes',getComputedStyle(doc('repos').querySelector('.search-toolbar')).display==='none');
  check('Graph uses compact sidebar presentation',doc('graph').body.classList.contains('sidebar'));
  check('Graph accepts its native theme',!doc('graph').getElementById('theme-status').textContent);
  check('Graph keeps colored lanes',getComputedStyle(doc('graph').querySelector('.lane span')).color!==getComputedStyle(doc('graph').body).color);
  check('Graph has its own vertical scroll area',doc('graph').getElementById('graph').scrollHeight>doc('graph').getElementById('graph').clientHeight);
  check('No empty commit details consume graph space',getComputedStyle(doc('graph').querySelector('.details')).display==='none');
  row('web').click();await tick();
  check('Repo selection updates Changes',doc('changes').body.textContent.includes('src/dashboard.ts')&&!doc('changes').body.textContent.includes('src/oauth.ts'));
  check('Repo selection preserves keyboard focus',doc('repos').activeElement.dataset.key==='repo:web');
  check('Selecting repos does not open an editor graph',!received.some(m=>m.type==='openGraph'));
  doc('changes').querySelector('.file-row').click();await tick();
  check('Changed files keep native diff routing and scope',received.some(m=>m.type==='openChange'&&m.id==='web-file'&&m.ownerId==='web'&&m.selectionEpoch===epoch));
  row('api').click();await tick();
  const worktreeButton=doc('repos').querySelector('[data-key="repo:api"] .worktree-action');
  worktreeButton.click();await tick();check('Disclosure keeps repository selection',activeOwner==='api');
  doc('repos').querySelector('[data-key="repo:api"] .worktree-action').click();await tick();
  row('api').click();await tick();
  doc('repos').getElementById('repositories').dispatchEvent(new KeyboardEvent('keydown',{key:'ArrowDown',bubbles:true}));await tick();
  check('Arrow selection follows linked worktrees',activeOwner==='auth'&&doc('changes').body.textContent.includes('src/oauth.ts'));
  row('api').click();await tick();retrying=true;win('changes').postMessage(snapshot(),'*');await tick();
  check('Changes shows its loading error',doc('changes').body.textContent.includes('Temporary status read failed.'));
  doc('changes').querySelector('.page-row').click();await tick();
  check('Changes Retry reloads its selected owner',!retrying&&doc('changes').querySelector('.file-row'));
  win('changes').postMessage({...snapshot(),errors:['Backend temporarily unavailable.']},'*');await tick();
  check('Changes surfaces shared errors even without the overview',doc('changes').querySelector('[role="alert"]').textContent.includes('Backend temporarily unavailable'));
  doc('changes').querySelector('.retry').click();await tick();
  check('Changes offers a scoped retry for shared errors',received.some(m=>m.surface==='changes'&&m.type==='refresh'&&m.ownerId===activeOwner&&m.selectionEpoch===epoch));
  win('changes').postMessage(snapshot(),'*');await tick();
  row('lib').click();await tick();
  check('Clean repos show a clear Changes empty state',doc('changes').body.textContent.includes('No changed files.'));
  check('Clean repos still show their commit graph',doc('graph').querySelectorAll('.commit').length===40);
  doc('graph').querySelector('.commit').click();await tick();
  check('Commit selection reveals its files',doc('graph').querySelector('.file'));
  doc('graph').querySelector('.file').click();await tick();
  check('Commit files keep native diff routing',received.some(m=>m.type==='openCommitFile'&&m.id==='commit-file'));
  for(const id of ['repos','changes','graph'])check('Webview has no outer horizontal overflow: '+id,
   doc(id).documentElement.scrollWidth<=doc(id).documentElement.clientWidth+1);
  row('api').click();await tick();
  document.body.dataset.result=btoa(JSON.stringify({ok:true,checks}));
 }catch(error){document.body.dataset.result=btoa(JSON.stringify({ok:false,checks,error:error.stack||String(error)}));}
})();});`;

async function chrome(url, screenshot, width, height) {
  const child = spawn(browser, ['--headless', '--disable-gpu', '--disable-background-networking', '--no-first-run', '--disable-extensions',
    '--remote-debugging-port=0', '--remote-debugging-address=127.0.0.1',
    `--user-data-dir=${path.join(temporary, path.basename(screenshot))}`, '--window-size=800,1000'],
  { stdio: ['ignore', 'ignore', 'pipe'] });
  const closed = new Promise(resolve => child.once('close', resolve));
  let stderr = '', socket;
  const timer = setTimeout(() => child.kill('SIGKILL'), 30000);
  try {
    const address = await new Promise((resolve, reject) => {
      child.stderr.on('data', chunk => {
        stderr += chunk;
        const match = stderr.match(/DevTools listening on (ws:\/\/[^\s]+)/);
        if (match) resolve(match[1]);
      });
      child.once('error', reject);
      child.once('close', code => reject(new Error(`Headless Chrome exited ${code}: ${stderr}`)));
    });
    socket = new WebSocket(address);
    await new Promise((resolve, reject) => { socket.onopen = resolve; socket.onerror = reject; });
    const pending = new Map(), events = new Map();
    let nextId = 0;
    const send = (method, params = {}, sessionId) => new Promise((resolve, reject) => {
      const id = ++nextId;
      pending.set(id, { resolve, reject });
      socket.send(JSON.stringify({ id, method, params, ...(sessionId ? { sessionId } : {}) }));
    });
    socket.onmessage = event => {
      const data = JSON.parse(event.data);
      if (data.id && pending.has(data.id)) {
        const request = pending.get(data.id);
        pending.delete(data.id);
        if (data.error) request.reject(new Error(JSON.stringify(data.error)));
        else request.resolve(data.result);
      } else {
        const key = `${data.sessionId || ''}:${data.method}`;
        events.get(key)?.resolve(data.params);
        events.delete(key);
      }
    };
    socket.onclose = () => {
      for (const request of pending.values()) request.reject(new Error(`Chrome connection closed: ${stderr}`));
      for (const request of events.values()) request.reject(new Error(`Chrome connection closed: ${stderr}`));
    };
    const { targetId } = await send('Target.createTarget', { url: 'about:blank' });
    const { sessionId } = await send('Target.attachToTarget', { targetId, flatten: true });
    await send('Page.enable', {}, sessionId);
    await send('Emulation.setDeviceMetricsOverride', { width, height, deviceScaleFactor: 1, mobile: false }, sessionId);
    const loaded = new Promise((resolve, reject) => events.set(`${sessionId}:Page.loadEventFired`, { resolve, reject }));
    await Promise.all([loaded, send('Page.navigate', { url }, sessionId)]);
    const evaluated = await send('Runtime.evaluate', { expression: `new Promise((resolve,reject)=>{
      const deadline=Date.now()+15000;const check=()=>{const result=document.body?.dataset.result;
      if(result)resolve(result);else if(Date.now()>deadline)reject(new Error('Stacked view timed out'));else setTimeout(check,20);};check();})`,
    awaitPromise: true, returnByValue: true }, sessionId);
    if (evaluated.exceptionDetails) throw new Error(JSON.stringify(evaluated.exceptionDetails));
    const capture = await send('Page.captureScreenshot', { format: 'png', captureBeyondViewport: false,
      clip: { x: 0, y: 0, width, height, scale: 1 } }, sessionId);
    fs.writeFileSync(screenshot, Buffer.from(capture.data, 'base64'));
    return JSON.parse(Buffer.from(evaluated.result.value, 'base64').toString('utf8'));
  } finally {
    socket?.close();
    child.kill('SIGTERM');
    await closed;
    clearTimeout(timer);
  }
}

async function run() {
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const base = `http://127.0.0.1:${server.address().port}`;
  const load = Module._load;
  let repoHtml, graphHtml;
  try {
    Module._load = function(name, ...args) {
      return name === 'vscode' ? { Uri: { joinPath: (...parts) => path.join(...parts) } } : load.call(this, name, ...args);
    };
    repoHtml = require('../../src/webview/html').getHtml;
    graphHtml = require('../../src/graph/html').getHtml;
  } finally { Module._load = load; }
  const webview = { cspSource: base, asWebviewUri: value => base + '/' + path.relative(extension, value).split(path.sep).join('/') };
  for (const theme of ['dark', 'light']) {
    const foreground = theme === 'dark' ? '#ddd' : '#333', background = theme === 'dark' ? '#1e1e1e' : '#fff';
    const colors = `:root{--vscode-sideBar-background:${background};--vscode-editor-background:${background};--vscode-sideBar-foreground:${foreground};--vscode-editor-foreground:${foreground};--vscode-foreground:${foreground};--vscode-list-inactiveSelectionBackground:${theme === 'dark' ? '#383838' : '#ddd'};--vscode-focusBorder:#4aadc4;--vscode-editor-font-size:13px;--vscode-editor-font-family:monospace;}`;
    add(`/${theme}/theme.css`, colors, 'text/css');
    add(`/${theme}/host.js`, scenario, 'text/javascript');
    for (const surface of ['repos', 'changes', 'graph']) {
      const bootstrap = `(()=>{window.posted=[];let state={};document.body.classList.add('vscode-${theme}');window.addEventListener('error',event=>parent.postMessage({surface:'${surface}',scriptError:event.message+' at '+event.filename+':'+event.lineno},'*'));window.acquireVsCodeApi=()=>({getState:()=>state,setState:value=>state=value,postMessage:message=>{window.posted.push(message);parent.postMessage({surface:'${surface}',message},'*');}});})();`;
      add(`/${theme}/${surface}-bootstrap.js`, bootstrap, 'text/javascript');
      let html = surface === 'graph' ? graphHtml(webview, extension, true) : repoHtml(webview, extension, surface === 'changes' ? 'changes' : 'repositories');
      html = html.replace('</head>', `<link rel="stylesheet" href="${base}/${theme}/theme.css"></head>`)
        .replace('<script src=', `<script src="${base}/${theme}/${surface}-bootstrap.js"></script><script src=`);
      add(`/${theme}/${surface}.html`, html);
    }
    add(`/${theme}/index.html`, `<!doctype html><html><head><meta charset="utf-8"><style>${colors}
      *{box-sizing:border-box}body{margin:0;padding:8px;background:${background};color:${foreground};font:13px monospace;height:100vh;display:grid;grid-template-rows:180px 240px minmax(0,1fr);gap:8px}
      section{display:flex;flex-direction:column;min-height:0;border:1px solid ${theme === 'dark' ? '#555' : '#bbb'}}
      h2{margin:0;padding:4px 8px;font:600 13px monospace;color:#419db3;border-bottom:1px solid ${theme === 'dark' ? '#444' : '#ccc'}}
      iframe{width:100%;flex:1;min-height:0;border:0}</style><script src="/${theme}/host.js"></script></head><body>
      <section><h2>Repositories</h2><iframe id="repos" src="/${theme}/repos.html" title="Repositories"></iframe></section>
      <section><h2 id="changes-heading">Changes</h2><iframe id="changes" src="/${theme}/changes.html" title="Changes"></iframe></section>
      <section><h2 id="graph-heading">Git Graph</h2><iframe id="graph" src="/${theme}/graph.html" title="Git Graph"></iframe></section></body></html>`);
    for (const narrow of theme === 'dark' ? [false, true] : [false]) {
      const label = `${theme}${narrow ? '-narrow' : ''}`;
      const screenshot = path.join(os.tmpdir(), `gitpane-stacked-${label}.png`);
      const result = await chrome(`${base}/${theme}/index.html`, screenshot, narrow ? 320 : 440, narrow ? 680 : 1000);
      if (!result.ok) throw new Error(`Stacked ${label} view failed after ${result.checks} checks:\n${result.error}`);
      console.log(`${label}: ${result.checks} stacked view checks passed; screenshot ${screenshot}`);
    }
  }
}

run().catch(error => { console.error(error.stack || String(error)); process.exitCode = 1; }).finally(() => {
  server.close();
  fs.rmSync(temporary, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });
});
