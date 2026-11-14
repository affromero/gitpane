'use strict';

// Record the real extension webviews with the sample data from the stacked
// browser fixture. Requires Node 22, Chrome, and ffmpeg.
const fs = require('node:fs');
const Module = require('node:module');
const path = require('node:path');

const root = path.resolve(__dirname, '..');
const fixture = path.join(root, 'vscode-extension/test/integration/stacked-browser.js');
let source = fs.readFileSync(fixture, 'utf8');

function replaceOnce(before, after) {
  if (!source.includes(before)) throw new Error(`Stacked fixture changed: ${before.slice(0, 80)}`);
  source = source.replace(before, after);
}

replaceOnce('Array.from({length:40}', 'Array.from({length:9}');
replaceOnce('short_id:prefix.repeat(6)+(i%10)',
  "short_id:['9fd2a10','b73e912','4c6d832','e41fa58','3a5d1c7','d85e02b','824f6a3','f07ca24','62b4e91'][i]");
replaceOnce('time:1793380000-i*3600', 'time:Math.floor(Date.now()/1000)-(i+1)*3600');
replaceOnce('apiRepo.worktrees=[linked];apiRepo.worktreeCount=1;', `
apiRepo.worktrees=[linked];apiRepo.worktreeCount=1;
apiRepo.stashCount=1;apiRepo.stashes=[{id:'draft',index:0,message:'Draft search improvements'}];
apiRepo.hasDirtySubmodules=true;
apiRepo.submodules=[{...owner('module','ui-module','main',true,
  [group('working','Unstaged',[file('module-file','src/theme.ts','M')])]),submodule:true}];`);
replaceOnce('const owners={api:apiRepo,auth:linked,web:webRepo,lib:cleanRepo};',
  'const owners={api:apiRepo,auth:linked,web:webRepo,lib:cleanRepo,module:apiRepo.submodules[0]};');
replaceOnce("const prefix={api:'a',auth:'b',web:'c',lib:'d'}[activeOwner];",
  "const prefix={api:'a',auth:'b',web:'c',lib:'d',module:'e'}[activeOwner];");
source = source.replaceAll('api-service', 'atlas-api').replaceAll('api-auth', 'atlas-search')
  .replaceAll('web-app', 'meadow-web').replaceAll('shared-lib', 'shared-ui')
  .replaceAll('Andres Romero', 'Alex')
  .replaceAll('Merge workspace updates', 'Merge search')
  .replaceAll('Keep worktree selection current', 'Update selection')
  .replaceAll('Open file changes in VS Code', 'Show file diff')
  .replaceAll('Refresh repository status', 'Refresh status')
  .replaceAll('Show linked worktrees', 'Add worktree')
  .replaceAll('Preserve graph lane colors', 'Color graph lanes')
  .replaceAll('Update selected repository', 'Select repository')
  .replaceAll('Load commit details', 'Show commit files');

// Keep the fixture's real webviews, theme, and sample snapshots. Replace its
// assertions with a short sequence of selections for the recording.
const start = source.indexOf("window.addEventListener('DOMContentLoaded',()=>{(async()=>{");
const end = source.indexOf('})();});`;', start);
if (start < 0 || end < 0) throw new Error('Stacked fixture scenario was not found');
source = source.slice(0, start) + String.raw`
window.addEventListener('DOMContentLoaded',()=>{(async()=>{
 const wait=()=>new Promise(resolve=>setTimeout(resolve,50));
 for(let n=0;n<200&&ready.size<3;n++)await wait();
 if(ready.size!==3)throw new Error('Webviews did not become ready');
 const row=id=>doc('repos').querySelector('[data-key="repo:'+id+'"] > .row');
 for(let n=0;n<100&&!row('web');n++)await wait();
 if(!row('web'))throw new Error('Sample repositories did not load');
 const cursor=document.createElement('div');
 cursor.style.cssText='position:fixed;z-index:999;left:50%;top:50%;width:14px;height:14px;border:2px solid white;border-radius:50%;box-shadow:0 1px 5px #000;pointer-events:none;transition:left .35s,top .35s';
 document.body.appendChild(cursor);
 const click=(frame,node)=>{
  const box=frame.getBoundingClientRect(),target=node.getBoundingClientRect();
  cursor.style.left=(box.left+target.left+Math.min(target.width/2,100))+'px';
  cursor.style.top=(box.top+target.top+target.height/2)+'px';
  setTimeout(()=>node.click(),400);
 };
 document.body.dataset.result=btoa(JSON.stringify({ok:true}));
 setTimeout(()=>click(document.getElementById('repos'),row('module')),2200);
 setTimeout(()=>click(document.getElementById('repos'),row('api').querySelector('.stash-action')),5200);
 setTimeout(()=>click(document.getElementById('repos'),doc('repos').querySelector('[data-key="worktree:auth"] > .row')),8200);
 setTimeout(()=>click(document.getElementById('repos'),row('api')),11200);
 setTimeout(()=>click(document.getElementById('graph'),doc('graph').querySelector('.commit')),14000);
})();});
` + source.slice(end + '})();});'.length);

replaceOnce("const { spawn } = require('node:child_process');",
  "const { spawn, spawnSync } = require('node:child_process');");
const captureStart = source.indexOf("    const capture = await send('Page.captureScreenshot'");
const captureEnd = source.indexOf('\n  } finally {', captureStart);
if (captureStart < 0 || captureEnd < 0) throw new Error('Stacked fixture capture was not found');
source = source.slice(0, captureStart) + String.raw`
    const result = JSON.parse(Buffer.from(evaluated.result.value, 'base64').toString('utf8'));
    if (!result.ok) return result;
    const frames = path.join(temporary, 'frames');
    fs.mkdirSync(frames);
    const start = Date.now();
    let frame = 0;
    while (Date.now() - start < 17500) {
      const capture = await send('Page.captureScreenshot', { format: 'png', captureBeyondViewport: false,
        clip: { x: 0, y: 0, width, height, scale: 1 } }, sessionId);
      fs.writeFileSync(path.join(frames, String(frame++).padStart(4, '0') + '.png'), Buffer.from(capture.data, 'base64'));
      const target = start + frame * 200;
      await new Promise(resolve => setTimeout(resolve, Math.max(0, target - Date.now())));
    }
    const output = path.resolve(extension, '../assets/demo-vscode.gif');
    const ffmpeg = spawnSync('ffmpeg', ['-v', 'error', '-y', '-framerate', '5', '-i', path.join(frames, '%04d.png'),
      '-filter_complex', '[0:v]split[a][b];[a]palettegen=stats_mode=diff[p];[b][p]paletteuse=dither=sierra2_4a',
      '-loop', '0', output], { encoding: 'utf8' });
    if (ffmpeg.error || ffmpeg.status !== 0) throw new Error(ffmpeg.error?.stack || ffmpeg.stderr);
    console.log('Recorded VS Code preview:', output);
    return result;` + source.slice(captureEnd);
replaceOnce("for (const theme of ['dark', 'light'])", "for (const theme of ['dark'])");
replaceOnce("for (const narrow of theme === 'dark' ? [false, true] : [false])", 'for (const narrow of [false])');
replaceOnce('narrow ? 320 : 440, narrow ? 680 : 1000', 'narrow ? 320 : 520, narrow ? 680 : 860');
replaceOnce('${label}: ${result.checks} stacked view checks passed; screenshot ${screenshot}',
  '${label}: sample webviews rendered');

const runner = new Module(fixture, module);
runner.filename = fixture;
runner.paths = Module._nodeModulePaths(path.dirname(fixture));
runner._compile(source, fixture);
