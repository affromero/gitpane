'use strict';

const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const { pathToFileURL } = require('node:url');

const extension = path.resolve(__dirname, '../..');
const browser = process.env.CHROME_EXECUTABLE || (process.platform === 'darwin'
  ? '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome' : 'google-chrome');
const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'gitpane-browser-test-'));
const stylesheet = fs.readFileSync(path.join(extension, 'src/webview/styles.css'), 'utf8');
const client = fs.readFileSync(path.join(extension, 'src/webview/view.js'), 'utf8');
const snapshot = {
  type: 'snapshot', errors: [], loading: false,
  repositories: [
    { id: 'api', name: 'api-service', branch: 'main', dirty: true, ahead: 2, behind: 1,
      groups: [
        { id: 'working', label: 'Unstaged', files: [
          { id: 'file1', status: 'M', path: 'src/server.rs' },
          { id: 'unsafe', status: '?', path: '</script><img src=x onerror="window.injected=true">' }
        ] },
        { id: 'index', label: 'Staged', files: [{ id: 'file2', status: 'M', path: 'src/server.rs' }] }
      ] },
    { id: 'web', name: 'web-app', branch: 'feature/ui', dirty: false, ahead: 0, behind: 0, groups: [] }
  ]
};
const encodedSnapshot = Buffer.from(JSON.stringify(snapshot)).toString('base64');
const scenario = String.raw`
(async () => {
  const checks = [];
  const check = (name, condition) => {
    if (!condition) throw new Error(name);
    checks.push(name);
  };
  const tick = () => new Promise(resolve => setTimeout(resolve, 30));
  const items = () => [...document.querySelectorAll('[role="treeitem"]')];
  const luminance = color => {
    const channels = color.match(/\d+/g).slice(0, 3).map(value => {
      const channel = Number(value) / 255;
      return channel <= 0.04045 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4;
    });
    return 0.2126 * channels[0] + 0.7152 * channels[1] + 0.0722 * channels[2];
  };
  const contrast = (first, second) => {
    const a = luminance(first);
    const b = luminance(second);
    return (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05);
  };
  const snapshot = JSON.parse(atob('SNAPSHOT_BASE64'));
  try {
    window.postMessage(snapshot, '*');
    await tick();
    const root = document.querySelector('[role="tree"]');
    check('tree label', root.getAttribute('aria-label') === 'Gitpane repositories');
    let repo = items().find(item => item.getAttribute('aria-label').includes('api-service'));
    check('repository status and branch', repo.textContent.includes('*') && repo.textContent.includes('main'));
    check('ahead and behind', repo.textContent.includes('↑2') && repo.textContent.includes('↓1'));
    check('unique file count', repo.textContent.includes('[2]'));
    check('collapsed by default', repo.getAttribute('aria-expanded') === 'false');
    repo.focus();
    repo.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowRight', bubbles: true }));
    repo = items().find(item => item.getAttribute('aria-label').includes('api-service'));
    check('keyboard expands repository', repo.getAttribute('aria-expanded') === 'true');
    check('persist expansion', window.savedState.expanded.includes('repo:api'));
    check('staged and unstaged visible', items().some(item => item.getAttribute('aria-label').startsWith('Staged')) && items().some(item => item.getAttribute('aria-label').startsWith('Unstaged')));
    check('unsafe path rendered as text', document.body.textContent.includes(snapshot.repositories[0].groups[0].files[1].path));
    check('unsafe path did not create markup', !document.querySelector('img') && !window.injected);
    repo.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowDown', bubbles: true }));
    check('arrow down selects group', document.activeElement.getAttribute('aria-label').startsWith('Unstaged'));
    document.activeElement.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowDown', bubbles: true }));
    check('arrow down selects file', document.activeElement.getAttribute('aria-label').includes('src/server.rs'));
    document.activeElement.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
    check('enter opens selected diff', window.posted.some(message => message.type === 'openChange' && message.id === 'file1'));
    const unsafe = items().find(item => item.getAttribute('aria-label').includes('<img'));
    unsafe.querySelector('.row').click();
    check('click opens selected diff', window.posted.some(message => message.type === 'openChange' && message.id === 'unsafe'));
    window.postMessage({ type: 'snapshot', repositories: [], errors: ['Bad <img>'], loading: false }, '*');
    await tick();
    check('error announced', document.querySelector('[role="alert"]').textContent === 'Bad <img>');
    document.querySelector('.retry').click();
    check('retry action', window.posted.some(message => message.type === 'refresh'));
    window.postMessage(snapshot, '*');
    await tick();
    repo = items().find(item => item.getAttribute('aria-label').includes('api-service'));
    check('expansion survives updates', repo.getAttribute('aria-expanded') === 'true');
    check('theme background applied', getComputedStyle(document.body).backgroundColor !== 'rgba(0, 0, 0, 0)');
    const background = getComputedStyle(document.body).backgroundColor;
    for (const selector of ['.dirty', '.branch.default', '.branch:not(.default)', '.ahead', '.behind', '.untracked']) {
      const node = document.querySelector(selector);
      check('fallback contrast ' + selector, !!node && contrast(getComputedStyle(node).color, background) >= 4.5);
    }
    check('visible tree has keyboard tab stop', items().filter(item => item.getAttribute('tabindex') === '0').length === 1);
    document.body.dataset.result = btoa(JSON.stringify({ ok: true, checks, background: getComputedStyle(document.body).backgroundColor }));
  } catch (error) {
    document.body.dataset.result = btoa(JSON.stringify({ ok: false, checks, error: error.stack || String(error) }));
  }
})();`;

function documentFor(theme) {
  const light = theme === 'light';
  const colors = light
    ? '--vscode-sideBar-background:#fff;--vscode-sideBar-foreground:#333;--vscode-list-inactiveSelectionBackground:#ddd;--vscode-focusBorder:#007acc;'
    : '--vscode-sideBar-background:#1e1e1e;--vscode-sideBar-foreground:#ddd;--vscode-list-inactiveSelectionBackground:#383838;--vscode-focusBorder:#007acc;';
  return `<!DOCTYPE html><html lang="en"><head><meta charset="utf-8"><style>:root {${colors}}\n${stylesheet}</style></head>
<body class="vscode-${theme}"><div id="messages" aria-live="polite"></div><div id="repositories" role="tree" aria-label="Gitpane repositories"></div>
<script>window.posted=[];window.savedState={};window.acquireVsCodeApi=()=>({postMessage:m=>window.posted.push(m),getState:()=>window.savedState,setState:s=>window.savedState=s});</script>
<script>${client}</script><script>${scenario.replace('SNAPSHOT_BASE64', encodedSnapshot)}</script></body></html>`;
}

function run(theme) {
  const fixture = path.join(temporary, `${theme}.html`);
  const profile = path.join(temporary, `profile-${theme}`);
  const screenshot = path.join(os.tmpdir(), `gitpane-webview-${theme}.png`);
  fs.writeFileSync(fixture, documentFor(theme));
  const args = ['--headless', '--disable-gpu', '--disable-background-networking', '--no-first-run',
    '--disable-extensions', `--user-data-dir=${profile}`, '--window-size=500,650', '--virtual-time-budget=4000'];
  const dom = spawnSync(browser, [...args, '--dump-dom', pathToFileURL(fixture).href], { encoding: 'utf8', timeout: 30000 });
  if (dom.error || dom.status !== 0) throw new Error(`Chrome ${theme} test failed: ${dom.error?.stack || dom.stderr}`);
  const encoded = dom.stdout.match(/data-result="([^"]+)"/)?.[1];
  if (!encoded) throw new Error(`Chrome ${theme} did not report test results:\n${dom.stdout}\n${dom.stderr}`);
  const result = JSON.parse(Buffer.from(encoded, 'base64').toString('utf8'));
  if (!result.ok) throw new Error(`Chrome ${theme} behavior failed: ${result.error}`);
  const image = spawnSync(browser, [...args, `--screenshot=${screenshot}`, pathToFileURL(fixture).href], { encoding: 'utf8', timeout: 30000 });
  if (image.error || image.status !== 0 || !fs.existsSync(screenshot)) throw new Error(`Chrome ${theme} screenshot failed: ${image.error?.stack || image.stderr}`);
  console.log(`${theme}: ${result.checks.length} checks passed; background ${result.background}; screenshot ${screenshot}`);
  return result;
}

try {
  const dark = run('dark');
  const light = run('light');
  if (dark.background === light.background) throw new Error('Light and dark theme backgrounds should differ.');
} catch (error) {
  console.error(error.stack || String(error));
  process.exitCode = 1;
} finally {
  fs.rmSync(temporary, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });
}
