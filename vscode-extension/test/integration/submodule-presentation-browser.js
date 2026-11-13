'use strict';

const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const { pathToFileURL } = require('node:url');

const extension = path.resolve(__dirname, '../..');
const browser = process.env.CHROME_EXECUTABLE || (process.platform === 'darwin'
  ? '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome' : 'google-chrome');
const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'gitpane-submodule-presentation-'));
const read = file => fs.readFileSync(path.join(extension, 'src', file), 'utf8');
const stylesheet = read('webview/styles.css');
const scripts = ['theme-webview.js', 'webview/file-menu.js', 'webview/stash-files.js',
  'webview/repository-label.js', 'webview/column-resize.js', 'webview/view.js'].map(read);

const scenario = String.raw`
(async () => {
  const checks = [];
  const check = (name, value) => { if (!value) throw new Error(name); checks.push(name); };
  const tick = () => new Promise(resolve => setTimeout(resolve, 30));
  const item = key => document.querySelector('[data-key="' + key + '"]');
  const row = key => item(key)?.querySelector(':scope > .row');
  const color = node => getComputedStyle(node).color;
  const highContrast = document.body.classList.contains('vscode-high-contrast') || document.body.classList.contains('vscode-high-contrast-light');
  const canvas = document.createElement('canvas').getContext('2d');
  const channels = value => {
    canvas.clearRect(0, 0, 1, 1);
    canvas.fillStyle = value;
    canvas.fillRect(0, 0, 1, 1);
    return [...canvas.getImageData(0, 0, 1, 1).data].slice(0, 3);
  };
  const luminance = value => channels(value).map(channel => {
    const c = channel / 255;
    return c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
  }).reduce((sum, channel, index) => sum + channel * [0.2126, 0.7152, 0.0722][index], 0);
  const contrast = (first, second) => {
    const a = luminance(first), b = luminance(second);
    return (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05);
  };
  const root = { id: 'root', name: 'root', branch: 'main', stashCount: 1, worktreeCount: 1, live: true,
    groups: [], stashes: [{ id: 'stash', index: 0, message: 'saved' }],
    worktrees: [{ id: 'ordinary', name: 'ordinary', branch: 'ordinary-branch', available: true,
      groups: [], submodules: [{ id: 'worktree-child', name: 'worktree-child', branch: 'main', submodule: true, groups: [] }] }],
    submodules: [{ id: 'module', name: 'module', branch: 'main', submodule: true, groups: [], worktreeCount: 2,
      worktrees: [{ id: 'internal', name: 'internal', branch: 'internal-branch', available: true, groups: [] },
        { id: 'outside', name: 'outside', branch: 'a-very-long-outside-branch-name', path: '/some/outside/worktree', external: true, available: false }],
      submodules: [{ id: 'nested', name: 'nested', branch: 'main', submodule: true, groups: [], worktreeCount: 1,
        worktrees: [{ id: 'nested-wt', name: 'nested-wt', branch: 'nested-branch', available: true, groups: [] }] }] }] };
  try {
    check('forced colors match the requested mode', matchMedia('(forced-colors: active)').matches === EXPECT_FORCED);
    check('contrast calculation rejects identical colors', contrast('rgb(128, 128, 128)', 'rgb(128, 128, 128)') === 1);
    const standalone = { id: 'standalone', name: 'a-very-long-standalone-repository-name', branch: 'main', groups: [], submodule: true };
    window.postMessage({ type: 'snapshot', repositories: [root, standalone], errors: [], expandWorktrees: true }, '*');
    await tick();
    const name = key => row(key).querySelector('.name');
    const weight = key => Number(getComputedStyle(name(key)).fontWeight);
    for (const key of ['repo:root', 'repo:standalone']) {
      check('top-level repository is bold: ' + key, weight(key) >= 700);
    }
    for (const key of ['repo:module', 'repo:nested', 'repo:worktree-child', 'worktree:ordinary', 'worktree:internal']) {
      check('nested repository or worktree keeps normal weight: ' + key, weight(key) === 400);
    }
    check('root submodule marker keeps normal weight', Number(getComputedStyle(name('repo:standalone').querySelector('.submodule')).fontWeight) === 400);
    const standaloneName = name('repo:standalone');
    check('long root name truncates and keeps its tooltip', standaloneName.scrollWidth > standaloneName.clientWidth && standaloneName.title === standalone.name && getComputedStyle(standaloneName).textOverflow === 'ellipsis');
    row('repo:standalone').click();
    check('focused root remains bold', document.activeElement === item('repo:standalone') && weight('repo:standalone') >= 700);
    document.getElementById('start-search').focus();
    check('selected root remains bold', item('repo:standalone').getAttribute('aria-selected') === 'true' && weight('repo:standalone') >= 700);
    const controls = [...row('repo:root').querySelector('.tail').children];
    const index = selector => controls.findIndex(node => node.matches(selector));
    check('disclosure indicators follow stash, worktree, submodule order',
      index('.stash-action') < index('.worktree-action') && index('.worktree-action') < index('.submodule-action') &&
      index('.submodule-action') < index('.live'));
    row('repo:root').querySelector('.stash-action').click();
    await tick();
    const children = [...item('repo:root').querySelector(':scope > .children').children].map(node => node.dataset.key);
    check('expanded entries follow stash, worktree, submodule order',
      children.indexOf('stash-group:root') < children.indexOf('worktree:ordinary') &&
      children.indexOf('worktree:ordinary') < children.indexOf('repo:module'));
    item('stash-group:root').focus();
    item('stash-group:root').dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowDown', bubbles: true }));
    check('keyboard navigation enters expanded stashes', item('stash:stash').getAttribute('aria-selected') === 'true');
    const sub = row('repo:module').querySelector('.name');
    const subControl = row('repo:root').querySelector('.submodule-action');
    const worktreeControl = row('repo:root').querySelector('.worktree-action');
    const subStyle = getComputedStyle(subControl), worktreeStyle = getComputedStyle(worktreeControl);
    check('submodule control matches neighboring background and border', subStyle.backgroundColor === worktreeStyle.backgroundColor && subStyle.borderStyle === worktreeStyle.borderStyle);
    if (!matchMedia('(forced-colors: active)').matches) check('submodule control has transparent background and no default border', subStyle.backgroundColor === 'rgba(0, 0, 0, 0)' && subStyle.borderStyle === 'none');
    check('submodule control matches neighboring font and spacing', subStyle.font === worktreeStyle.font && subStyle.padding === worktreeStyle.padding);
    check('submodule control is keyboard accessible and labeled', subControl.tabIndex === 0 && /submodules/i.test(subControl.getAttribute('aria-label')) && subControl.getAttribute('aria-expanded') === 'true');
    subControl.focus();
    check('submodule control shows keyboard focus', document.activeElement === subControl && subControl.matches(':focus-visible') && getComputedStyle(subControl).outlineStyle !== 'none');
    for (const key of ['worktree:internal', 'worktree:outside', 'worktree:nested-wt']) {
      check('submodule worktree name uses submodule color for ' + key, color(row(key).querySelector('.name')) === color(sub));
    }
    if (!highContrast && !matchMedia('(forced-colors: active)').matches) {
      check('ordinary worktree name retains its color', color(row('worktree:ordinary').querySelector('.name')) !== color(sub));
      check('worktree marker retains its color', color(row('worktree:internal').querySelector('.subtree')) !== color(sub));
      check('worktree branch retains its color', color(row('worktree:internal').querySelector('.branch')) !== color(sub));
      check('repository disclosures use distinct colors', color(row('repo:root').querySelector('.submodule-action')) !== color(row('repo:root').querySelector('.worktree-action')));
      check('worktree submodule disclosure uses the same softened submodule color', color(row('worktree:ordinary').querySelector('.submodule-action')) === color(subControl));
      const distance = (first, second) => Math.hypot(...channels(first).map((channel, index) => channel - channels(second)[index]));
      check('submodule disclosure color is softened toward the foreground', distance(color(subControl), color(row('repo:root'))) < distance(color(sub), color(row('repo:root'))));
      check('submodule disclosure is readable on the normal background', contrast(color(subControl), getComputedStyle(document.body).backgroundColor) >= 4.5);
      item('repo:root').focus();
      check('selected submodule disclosure is readable', contrast(color(subControl), getComputedStyle(row('repo:root')).backgroundColor) >= 4.5);
    }
    const outside = row('worktree:outside').querySelector('.name');
    check('outside worktree tooltip includes path and full branch', outside.title.includes('/some/outside/worktree') && outside.title.includes('a-very-long-outside-branch-name'));
    if (highContrast || matchMedia('(forced-colors: active)').matches) {
      item('worktree:internal').focus();
      check('selected submodule worktree name uses selection foreground', color(row('worktree:internal').querySelector('.name')) === color(row('worktree:internal')));
    }
    row('repo:root').querySelector('.submodule-action').click();
    check('submodule disclosure collapses independently', !item('repo:module') && !!item('worktree:ordinary') && !!item('stash-group:root'));
    row('repo:root').querySelector('.submodule-action').click();
    row('repo:root').querySelector('.worktree-action').click();
    check('worktree disclosure collapses independently', !item('worktree:ordinary') && !!item('repo:module') && !!item('stash-group:root'));
    root.statusUnavailable = true;
    window.postMessage({ type: 'snapshot', repositories: [root], errors: [], expandWorktrees: true }, '*');
    await tick();
    check('unavailable status retains submodule control', !!row('repo:root').querySelector('.submodule-action'));
    check('unavailable status hides stale stash and worktree controls', !row('repo:root').querySelector('.stash-action') && !row('repo:root').querySelector('.worktree-action'));
    if (highContrast || matchMedia('(forced-colors: active)').matches) {
      item('repo:root').focus();
      check('selected submodule control uses selection foreground', color(row('repo:root').querySelector('.submodule-action')) === color(row('repo:root')));
    }
    document.body.dataset.result = btoa(JSON.stringify({ ok: true, checks }));
  } catch (error) { document.body.dataset.result = btoa(JSON.stringify({ ok: false, checks, error: String(error.stack || error) })); }
})();`;

try {
  for (const mode of ['dark', 'light', 'custom', 'high-contrast', 'high-contrast-light', 'forced']) {
    const bodyClass = mode === 'forced' ? 'vscode-dark' : mode === 'custom' ? 'vscode-dark' : `vscode-${mode}`;
    const colors = mode === 'custom' ? '--vscode-terminal-ansiMagenta:rgb(154, 70, 150);--vscode-terminal-ansiYellow:rgb(150, 110, 20);' : '';
    const light = mode === 'light' || mode === 'high-contrast-light';
    const html = `<!DOCTYPE html><html><head><meta charset="utf-8"><style>:root{${colors}--vscode-sideBar-background:${light ? '#fff' : '#1e1e1e'};--vscode-sideBar-foreground:${light ? '#222' : '#ddd'};--vscode-list-activeSelectionBackground:#315e8a;--vscode-list-activeSelectionForeground:#fff;--vscode-list-inactiveSelectionBackground:#315e8a;--vscode-list-inactiveSelectionForeground:#fff;--vscode-focusBorder:#fff;}${stylesheet}</style></head>
      <body class="${bodyClass}"><div id="messages"></div><button id="start-search"></button><div id="search-box" hidden><input id="file-search"><button id="search-prev"></button><button id="search-next"></button><div id="search-status"></div></div><div id="repositories" role="tree" style="width:210px"></div>
      <script>window.acquireVsCodeApi=()=>({postMessage(){},getState(){return {}},setState(){}});</script>${scripts.map(script => `<script>${script}</script>`).join('')}<script>${scenario.replace('EXPECT_FORCED', String(mode === 'forced'))}</script></body></html>`;
    const fixture = path.join(temporary, `${mode}.html`);
    fs.writeFileSync(fixture, html);
    const args = ['--headless', '--disable-gpu', '--disable-background-networking', '--no-first-run', '--disable-extensions',
      `--user-data-dir=${path.join(temporary, `profile-${mode}`)}`, '--window-size=500,650', '--virtual-time-budget=4000',
      ...(mode === 'forced' ? ['--force-high-contrast'] : []), '--dump-dom', pathToFileURL(fixture).href];
    const result = spawnSync(browser, args, { encoding: 'utf8', timeout: 30000 });
    if (result.error || result.status !== 0) throw new Error(`Chrome ${mode} failed: ${result.error?.stack || result.stderr}`);
    const encoded = result.stdout.match(/data-result="([^"]+)"/)?.[1];
    if (!encoded) throw new Error(`Chrome ${mode} did not report results: ${result.stderr}`);
    const report = JSON.parse(Buffer.from(encoded, 'base64').toString('utf8'));
    if (!report.ok) throw new Error(`${mode} failed after ${report.checks.length} checks: ${report.error}`);
    console.log(`${mode}: ${report.checks.length} submodule presentation checks passed`);
  }
} catch (error) { console.error(error.stack || String(error)); process.exitCode = 1; }
finally { fs.rmSync(temporary, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 }); }
