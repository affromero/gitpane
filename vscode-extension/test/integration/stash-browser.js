'use strict';

// Browser regression for stash browsing in the Explorer webview. Kept separate
// from test/integration/browser.js, which is at the repository file-size limit.
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const { pathToFileURL } = require('node:url');

const extension = path.resolve(__dirname, '../..');
const browser = process.env.CHROME_EXECUTABLE || (process.platform === 'darwin'
  ? '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome' : 'google-chrome');
const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'gitpane-stash-browser-'));
const stylesheet = fs.readFileSync(path.join(extension, 'src/webview/styles.css'), 'utf8');
const client = fs.readFileSync(path.join(extension, 'src/webview/view.js'), 'utf8');
const fileMenuClient = fs.readFileSync(path.join(extension, 'src/webview/file-menu.js'), 'utf8');
const stashClient = fs.readFileSync(path.join(extension, 'src/webview/stash-files.js'), 'utf8');

const repo = (stash) => ({ id: 'repo1', name: 'api-service', branch: 'main', dirty: true, stashCount: 1,
  stashes: [stash], groups: [{ id: 'working', label: 'Unstaged', files: [{ id: 'change1', status: 'M', path: 'src/server.rs' }] }] });
const snapshot = stash => ({ type: 'snapshot', errors: [], loading: false, expandWorktrees: false,
  repositories: [repo(stash)] });
const encode = value => Buffer.from(JSON.stringify(value)).toString('base64');
const initial = snapshot({ id: 'stash1', index: 0, message: 'WIP on main' });

const scenario = `
(async () => {
  const checks = [];
  try {
  const check = (name, condition) => { if (!condition) throw new Error(name); checks.push(name); };
  const tick = () => new Promise(resolve => setTimeout(resolve, 30));
  const post = value => window.postMessage(value, '*');
  const items = () => [...document.querySelectorAll('[role="treeitem"]')];
  const byKey = key => items().find(item => item.dataset.key === key);
  const posted = () => window.posted;
  const last = type => [...posted()].reverse().find(message => message.type === type);
  window.posted = [];

  post(${JSON.stringify(initial)});
  await tick();

  const stashButton = document.querySelector('.stash-action');
  check('stash button is offered for the repository', !!stashButton);
  stashButton.click();
  await tick();
  check('stash group expands', !!byKey('stash-group:repo1'));
  const stash = byKey('stash:stash1');
  check('stash nodes are expandable', !!stash && stash.getAttribute('aria-expanded') === 'false');

  stash.querySelector('.row').click();
  await tick();
  check('expanding a stash requests its saved files', posted().some(message => message.type === 'loadStashFiles' && message.id === 'stash1'));
  const loadingStash = byKey('stash:stash1');
  check('the stash node reports loading', !!loadingStash?.querySelector('.page-status') && loadingStash.getAttribute('aria-expanded') === 'true');

  post(${JSON.stringify(snapshot({ id: 'stash1', index: 0, message: 'WIP on main', fileState: 'ready', files: [
    { id: 'f1', status: 'M', path: 'src/server.rs', oldPath: 'src/server.rs', label: 'M src/server.rs', binary: false, gitlink: false },
    { id: 'f2', status: 'R', path: 'new name.txt', oldPath: 'old name.txt', label: 'R old name.txt → new name.txt', binary: false, gitlink: false },
    { id: 'f3', status: 'D', path: 'gone.txt', oldPath: 'gone.txt', label: 'D gone.txt', binary: false, gitlink: false },
    { id: 'f4', status: 'A', path: 'saved.txt', oldPath: null, label: 'A saved.txt', binary: true, gitlink: false }
  ] }))});
  await tick();
  check('saved files render under the stash', !!byKey('stash-file:f1') && !!byKey('stash-file:f2'));
  check('modified files display their path once', byKey('stash-file:f1').querySelector('.file-path').textContent === 'src/server.rs');
  const rename = byKey('stash-file:f2').textContent;
  check('renames show both saved paths', rename.includes('old name.txt → new name.txt'));
  check('deleted files stay visible', byKey('stash-file:f3').textContent.includes('gone.txt'));
  check('binary files are marked', byKey('stash-file:f4').textContent.includes('binary'));

  byKey('stash-file:f1').querySelector('.row').click();
  await tick();
  const open = last('openStashFile');
  check('clicking a saved file opens the native diff', !!open && open.id === 'stash1' && open.fileId === 'f1');

  post(${JSON.stringify(snapshot({ id: 'stash1', index: 0, message: 'WIP on main', fileState: 'error', fileError: 'The saved stash is no longer available.' }))});
  await tick();
  const alert = document.querySelector('[role="alert"]');
  check('read failures are visible', !!alert && alert.textContent.includes('no longer available'));

  post(${JSON.stringify(snapshot({ id: 'stash1', index: 1, message: 'WIP on main', fileState: 'empty', files: [] }))});
  await tick();
  check('a renumbered stash keeps its own state', !!byKey('stash:stash1') && document.body.textContent.includes('No saved files.'));

  post(${JSON.stringify(snapshot({ id: 'stash1', index: 1, message: 'WIP on main', fileState: 'ready', files: [
    { id: 'f9', status: 'A', path: 'saved.txt', oldPath: null, label: 'A saved.txt', binary: false, gitlink: false }
  ] }))});
  await tick();
  check('a renumbered stash never shows another stash files', !!byKey('stash-file:f9') && !byKey('stash-file:f1'));

  document.body.dataset.result = btoa(JSON.stringify({ ok: true, checks }));
} catch (error) {
  document.body.dataset.result = btoa(JSON.stringify({ ok: false, checks, error: String(error.stack || error) }));
}
})();`;

const documentFor = () => `<!DOCTYPE html><html lang="en"><head><meta charset="utf-8">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'nonce-gitpane-test'; script-src 'nonce-gitpane-test'">
<style nonce="gitpane-test">:root{--vscode-sideBar-background:#1e1e1e;--vscode-sideBar-foreground:#ddd;--vscode-focusBorder:#007acc;}${stylesheet}</style></head>
<body class="vscode-dark"><div id="messages" aria-live="polite"></div><div class="search-toolbar"><button id="start-search" type="button">⌕</button><div id="search-box" hidden><input id="file-search" type="search"><button id="search-prev" type="button" disabled>↑</button><button id="search-next" type="button" disabled>↓</button><div id="search-status" role="status"></div></div></div><div id="repositories" role="tree" aria-label="Gitpane repositories"></div>
<script nonce="gitpane-test">window.posted=[];window.savedState={};window.acquireVsCodeApi=()=>({postMessage:m=>window.posted.push(m),getState:()=>window.savedState,setState:s=>window.savedState=s});</script>
<script nonce="gitpane-test">${fileMenuClient}</script><script nonce="gitpane-test">${stashClient}</script><script nonce="gitpane-test">${client}</script><script nonce="gitpane-test">${scenario}</script></body></html>`;

const fixture = path.join(temporary, 'stash.html');
fs.writeFileSync(fixture, documentFor());
const args = ['--headless', '--disable-gpu', '--disable-background-networking', '--no-first-run', '--disable-extensions',
  `--user-data-dir=${path.join(temporary, 'profile')}`, '--window-size=500,650', '--virtual-time-budget=4000'];
const dom = spawnSync(browser, [...args, '--dump-dom', pathToFileURL(fixture).href], { encoding: 'utf8', timeout: 30000 });
try {
  if (dom.error || dom.status !== 0) throw new Error(`Chrome stash test failed: ${dom.error?.stack || dom.stderr}`);
  const encoded = dom.stdout.match(/data-result="([^"]+)"/)?.[1];
  if (!encoded) throw new Error(`Chrome did not report stash results:\n${dom.stdout}\n${dom.stderr}`);
  const result = JSON.parse(Buffer.from(encoded, 'base64').toString('utf8'));
  if (!result.ok) throw new Error(`Stash browsing failed after ${result.checks.length} checks: ${result.error}`);
  console.log(`${result.checks.length} stash browsing checks passed`);
} finally {
  fs.rmSync(temporary, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });
}
