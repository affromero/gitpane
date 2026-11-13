'use strict';

const fs = require('node:fs');
const http = require('node:http');
const Module = require('node:module');
const os = require('node:os');
const path = require('node:path');
const { spawn } = require('node:child_process');

const extension = path.resolve(__dirname, '../..');
const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'gitpane-unified-browser-'));
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
for (const file of ['webview/styles.css', 'webview/view.js', 'webview/file-menu.js', 'webview/stash-files.js', 'webview/repository-label.js', 'webview/column-resize.js', 'graph/styles.css', 'graph/view.js',
  'theme-webview.js', 'unified/styles.css', 'unified/view.js', 'unified/frame.js']) {
  add(`/src/${file}`, fs.readFileSync(path.join(extension, 'src', file)), file.endsWith('.css') ? 'text/css' : 'text/javascript');
}

const bootstrap = String.raw`
window.received = [];
window.saved = { paneSizes: [0.25, 0.35, 0.4], views: {} };
window.acquireVsCodeApi = () => ({
 getState: () => window.saved,
 setState: value => { window.saved = value; },
 postMessage: packet => { window.received.push(packet); }
});`;

const scenario = String.raw`
(async () => {
 let checks = 0;
 const check = (label, condition) => { if (!condition) throw new Error(label); checks++; };
 const tick = () => new Promise(resolve => setTimeout(resolve, 30));
 const frame = name => document.querySelector('iframe[data-surface="' + name + '"]');
 const doc = name => frame(name).contentDocument;
 const host = document.createElement('iframe');
 document.body.appendChild(host);
 const send = (surface, message) => window.dispatchEvent(new MessageEvent('message',
  { data: { surface, message }, source: host.contentWindow }));
 const group = { id: 'working', label: 'Unstaged', files: [{ id: 'file', status: 'M', path: 'changed.txt' }] };
 const owner = { id: 'repo', name: 'repo', branch: 'main', dirty: true, groups: [group], worktrees: [],
  changesPage: { state: 'ready', page: 0, previous: false, next: false } };
 owner.worktrees.push({ id: 'external', name: 'long-external-worktree', branch: 'feature/review', external: true,
  available: false, dirty: true, fileCount: 2, groups: [] });
 const snapshot = { type: 'snapshot', repositories: [owner], selectedOwner: owner, selectedOwnerId: owner.id,
  selectedKind: 'repo', selectionEpoch: 7, separateChanges: true, expandWorktrees: true, errors: [], loading: false };
 try {
  for (let n = 0; n < 100 && new Set(received.filter(p => p.message.type === 'ready').map(p => p.surface)).size < 3; n++) await tick();
  check('three frames initialize independently', new Set(received.filter(p => p.message.type === 'ready').map(p => p.surface)).size === 3);
  check('three titled sections exist', ['repositories','changes','graph'].every(name => frame(name).title));
  check('stored pane sizes are restored', Math.abs(document.getElementById('repositories-pane').getBoundingClientRect().height /
    [...document.querySelectorAll('.pane')].reduce((sum, pane) => sum + pane.getBoundingClientRect().height, 0) - 0.25) < 0.03);
  for (const surface of ['repositories', 'changes']) send(surface, snapshot);
  send('graph', { type: 'graph', repoName: 'repo', rows: [{ oid: 'a'.repeat(40), short_id: 'aaaaaaa', message: 'first commit',
   author: 'Test', time: 1, is_merge: false, commit_col: 0, lanes: ['commit'], parent_oids: [], labels: [] }],
   loading: false, firstParent: false, filters: { branches: null, authors: null, refs: {local:true,remote:true,tags:true,stashes:true} } });
  await tick(); await tick();
  check('repositories snapshot reaches repository pane', doc('repositories').body.textContent.includes('repo'));
  check('selected changes reach changes pane', doc('changes').body.textContent.includes('changed.txt'));
  check('graph snapshot reaches graph pane', doc('graph').body.textContent.includes('first commit'));
  let repoDoc = doc('repositories');
  let handles = repoDoc.querySelectorAll('.column-separator');
  check('repository columns have accessible resize controls', handles.length === 2 &&
   [...handles].every(handle => handle.getAttribute('aria-label') && handle.getAttribute('aria-orientation') === 'vertical'));
  const nameWidth = repoDoc.querySelector('.repo-row .name').getBoundingClientRect().width;
  handles[0].focus();
  handles[0].dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowRight', bubbles: true }));
  check('keyboard resizing changes the name column and saves its width',
   repoDoc.querySelector('.repo-row .name').getBoundingClientRect().width > nameWidth &&
   saved.views.repositories.columnWidths[0] > nameWidth);
  const branchWidth = repoDoc.querySelector('.repo-row .branch').getBoundingClientRect().width;
  handles[1].setPointerCapture = () => {};
  handles[1].dispatchEvent(new PointerEvent('pointerdown', { button: 0, pointerId: 1, clientX: 100, bubbles: true }));
  handles[1].dispatchEvent(new PointerEvent('pointermove', { pointerId: 1, clientX: 90, bubbles: true }));
  handles[1].dispatchEvent(new PointerEvent('pointerup', { pointerId: 1, clientX: 90, bubbles: true }));
  check('dragging resizes the branch column', repoDoc.querySelector('.repo-row .branch').getBoundingClientRect().width < branchWidth);
  send('repositories', { ...snapshot, fontSize: 20 }); await tick();
  check('refresh keeps resize focus and preferences', repoDoc.activeElement === handles[0] &&
   saved.views.repositories.columnWidths.length === 2);
  let preferredWidths = [...saved.views.repositories.columnWidths];
  frame('repositories').style.width = '210px';
  await tick(); await tick();
  check('narrowing preserves the saved widths and keeps the row within the panel',
   JSON.stringify(saved.views.repositories.columnWidths) === JSON.stringify(preferredWidths) &&
   repoDoc.querySelector('.repo-row').getBoundingClientRect().right <= repoDoc.documentElement.clientWidth + 1);
  const externalRow = repoDoc.querySelector('.external-worktree-row');
  const externalTail = externalRow.querySelector('.tail').getBoundingClientRect();
  const parentTail = repoDoc.querySelector('.repo-row .tail').getBoundingClientRect();
  check('outside worktree status aligns and actions remain visible in a narrow panel',
   Math.abs(externalTail.left - parentTail.left) < 1 && [...externalRow.querySelector('.tail').children].every(item => {
    const element = externalRow.querySelector('.tail');
    element.scrollLeft += Math.max(0, item.getBoundingClientRect().right - externalTail.right);
    const bounds = item.getBoundingClientRect();
    return bounds.left >= externalTail.left - 1 && bounds.right <= externalTail.right + 1;
   }));
  const scrollTail = externalRow.querySelector('.tail');
  scrollTail.focus();
  const scrollKey = new KeyboardEvent('keydown', { key: 'ArrowRight', bubbles: true, cancelable: true });
  scrollTail.dispatchEvent(scrollKey);
  check('status scrolling preserves keyboard access', repoDoc.activeElement === scrollTail && !scrollKey.defaultPrevented);
  send('repositories', { ...snapshot, fontSize: 20, statusAlignment: 'left' }); await tick();
  check('refresh restores focus to status controls', repoDoc.activeElement.classList.contains('tail'));
  handles[0].dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowLeft', bubbles: true }));
  check('resizing a narrow name column preserves the saved branch width',
   saved.views.repositories.columnWidths[1] === preferredWidths[1]);
  preferredWidths = [...saved.views.repositories.columnWidths];
  send('repositories', { ...snapshot, fontSize: 40 }); await tick(); await tick();
  const largeRow = repoDoc.querySelector('.external-worktree-row');
  check('large text keeps name visible', repoDoc.querySelector('.repo-row .name').getBoundingClientRect().width > 0 &&
   repoDoc.querySelector('.repo-row .branch').getBoundingClientRect().width > 0);
  check('large text keeps worktree status on one line inside the panel',
   repoDoc.documentElement.scrollWidth <= repoDoc.documentElement.clientWidth + 1 &&
   [...largeRow.querySelector('.tail').children].every(item =>
    Math.abs(item.getBoundingClientRect().top - largeRow.querySelector('.tail').firstElementChild.getBoundingClientRect().top) < 1));
  send('repositories', { ...snapshot, fontSize: 20 }); await tick();
  frame('repositories').style.width = '';
  await tick(); await tick();
  check('widening restores the chosen width',
   Math.abs(repoDoc.querySelector('.repo-row .name').getBoundingClientRect().width - preferredWidths[0]) < 1);
  const repoFrame = frame('repositories');
  const reloaded = new Promise(resolve => repoFrame.addEventListener('load', resolve, { once: true }));
  repoFrame.srcdoc = repoFrame.srcdoc;
  await reloaded;
  send('repositories', snapshot); await tick(); await tick();
  repoDoc = doc('repositories');
  handles = repoDoc.querySelectorAll('.column-separator');
  check('recreating the view restores the chosen widths', handles.length === 2 &&
   Math.abs(repoDoc.querySelector('.repo-row .name').getBoundingClientRect().width - preferredWidths[0]) < 1);
  repoDoc.querySelector('.column-reset').click();
  check('reset restores automatic column widths', saved.views.repositories.columnWidths === null);
  send('repositories', { ...snapshot, fontSize: 20 }); await tick();
  check('font size applies to all three panes and their headings',
   getComputedStyle(document.querySelector('.pane h2')).fontSize === '20px' &&
   ['repositories', 'changes', 'graph'].every(name => getComputedStyle(doc(name).body).fontSize === '20px'));
  send('repositories', { ...snapshot, fontSize: 0 }); await tick();
  check('automatic font size restores the editor setting',
   !document.documentElement.style.getPropertyValue('--gitpane-font-size') &&
   ['repositories', 'changes', 'graph'].every(name => !doc(name).documentElement.style.getPropertyValue('--gitpane-font-size')));
  doc('changes').querySelector('.file-row').click(); await tick();
  check('diff action retains owner and selection epoch', received.some(packet => packet.surface === 'changes' &&
   packet.message.type === 'openChange' && packet.message.id === 'file' && packet.message.ownerId === 'repo' && packet.message.selectionEpoch === 7));
  const rightClick = new MouseEvent('contextmenu', { bubbles: true, cancelable: true, button: 2, clientX: 45, clientY: 55 });
  doc('changes').querySelector('.file-row .file-path').dispatchEvent(rightClick); await tick();
  check('right click selects the file and requests its guarded menu', rightClick.defaultPrevented &&
   doc('changes').querySelector('.file-row').parentElement.getAttribute('aria-selected') === 'true' &&
   received.at(-1).surface === 'changes' && received.at(-1).message.type === 'fileMenuRequest' &&
   received.at(-1).message.id === 'file' &&
   received.at(-1).message.ownerId === 'repo' && received.at(-1).message.selectionEpoch === 7);
  const requestId = received.at(-1).message.requestId;
  send('changes', { type: 'fileMenu', id: 'file', requestId, token: 'test-token',
   choices: [{ label: 'Open Changes', action: 'open' }, { label: 'Stage', action: 'stage' }] });
  await tick();
  const popup = doc('changes').querySelector('[role="menu"]');
  const bounds = popup?.getBoundingClientRect();
  check('file menu opens beside the pointer', popup && bounds.left === 45 && bounds.top === 55 &&
   [...popup.querySelectorAll('button')].map(button => button.textContent).join(',') === 'Open Changes,Stage');
  popup.querySelectorAll('button')[1].click(); await tick();
  check('menu action carries the selected file and guard', received.at(-1).message.type === 'fileMenuAction' &&
   received.at(-1).message.id === 'file' && received.at(-1).message.action === 'stage' &&
   received.at(-1).message.token === 'test-token' && !doc('changes').querySelector('[role="menu"]'));
  doc('changes').querySelector('.file-row .menu-action').click(); await tick();
  check('file ellipsis opens the same local menu', received.at(-1).message.type === 'fileMenuRequest' &&
   received.at(-1).message.id === 'file');
  send('changes', { type: 'fileMenu', id: 'file', requestId: received.at(-1).message.requestId,
   token: 'second-token', choices: [{ label: 'Open Changes', action: 'open' }] }); await tick();
  doc('changes').dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
  check('escape closes the file menu and returns focus to the file', !doc('changes').querySelector('[role="menu"]') &&
   doc('changes').activeElement.getAttribute('role') === 'treeitem');
  const splitter = document.querySelector('.splitter');
  const before = document.getElementById('repositories-pane').getBoundingClientRect().height;
  splitter.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowDown', bubbles: true }));
  check('keyboard splitter resizes and persists the pane', document.getElementById('repositories-pane').getBoundingClientRect().height > before &&
   Array.isArray(saved.paneSizes) && saved.paneSizes[0] > 0.25 && splitter.getAttribute('aria-valuenow'));
  const second = document.querySelectorAll('.splitter')[1];
  const changesBefore = document.getElementById('changes-pane').getBoundingClientRect().height;
  const changesSizeBefore = saved.paneSizes[1];
  // Synthetic pointer events cannot acquire a browser pointer capture.
  second.setPointerCapture = () => {};
  second.dispatchEvent(new PointerEvent('pointerdown', { button: 0, pointerId: 1, clientY: 200, bubbles: true }));
  second.dispatchEvent(new PointerEvent('pointermove', { pointerId: 1, clientY: 220, bubbles: true }));
  second.dispatchEvent(new PointerEvent('pointerup', { pointerId: 1, clientY: 220, bubbles: true }));
  check('pointer drag resizes and persists the adjacent panes', document.getElementById('changes-pane').getBoundingClientRect().height > changesBefore &&
   saved.paneSizes[1] > changesSizeBefore);
  document.body.classList.add('vscode-high-contrast'); await tick();
  check('theme class propagates to all panes', ['repositories','changes','graph'].every(name => doc(name).body.classList.contains('vscode-high-contrast')));
  const beforeState = JSON.stringify(saved.paneSizes);
  window.GitpaneFrameApi('repositories').setState({ expanded: ['repo:repo'] });
  window.GitpaneFrameApi('graph').setState({ search: 'needle' });
  check('pane states and sizes remain independent', JSON.stringify(saved.paneSizes) === beforeState &&
   saved.views.repositories.expanded[0] === 'repo:repo' && saved.views.graph.search === 'needle');
  document.body.dataset.result = btoa(JSON.stringify({ ok: true, checks }));
 } catch (error) { document.body.dataset.result = btoa(JSON.stringify({ ok: false, checks, error: String(error.stack || error) })); }
})();`;

async function run() {
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const base = `http://127.0.0.1:${server.address().port}`;
  const load = Module._load;
  let getHtml;
  try {
    Module._load = function(name, ...args) {
      return name === 'vscode' ? { Uri: { joinPath: (...parts) => path.join(...parts) } } : load.call(this, name, ...args);
    };
    ({ getHtml } = require('../../src/unified/html'));
  } finally { Module._load = load; }
  const webview = { cspSource: base, asWebviewUri: value => base + '/' + path.relative(extension, value).split(path.sep).join('/') };
  add('/bootstrap.js', bootstrap, 'text/javascript');
  add('/scenario.js', scenario, 'text/javascript');
  const html = getHtml(webview, extension).replace('<script src=', '<script src="/bootstrap.js"></script><script src=')
    .replace('</body>', '<script src="/scenario.js"></script></body>');
  add('/index.html', html);
  const child = spawn(browser, ['--headless', '--disable-gpu', '--disable-background-networking', '--no-first-run',
    '--disable-extensions', `--user-data-dir=${path.join(temporary, 'profile')}`, '--window-size=440,1000',
    '--virtual-time-budget=10000', '--dump-dom', `${base}/index.html`], { stdio: ['ignore', 'pipe', 'pipe'] });
  let stdout = '', stderr = '';
  child.stdout.on('data', chunk => { stdout += chunk; });
  child.stderr.on('data', chunk => { stderr += chunk; });
  const timeout = setTimeout(() => child.kill('SIGKILL'), 30000);
  const code = await new Promise((resolve, reject) => { child.once('error', reject); child.once('close', resolve); });
  clearTimeout(timeout);
  if (code !== 0) throw new Error(`Chrome exited ${code}: ${stderr}`);
  const encoded = stdout.match(/data-result="([^"]+)"/)?.[1];
  if (!encoded) throw new Error(`Unified view did not report results: ${stderr}`);
  const result = JSON.parse(Buffer.from(encoded, 'base64').toString('utf8'));
  if (!result.ok) throw new Error(`Unified view failed after ${result.checks} checks: ${result.error}`);
  console.log(`${result.checks} unified webview checks passed`);
}

run().catch(error => { console.error(error.stack || String(error)); process.exitCode = 1; }).finally(() => {
  server.close();
  fs.rmSync(temporary, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });
});
