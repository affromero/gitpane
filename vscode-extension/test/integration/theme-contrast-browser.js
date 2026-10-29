'use strict';

const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const { pathToFileURL } = require('node:url');

const extension = path.resolve(__dirname, '../..');
const browser = process.env.CHROME_EXECUTABLE || (process.platform === 'darwin'
  ? '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome' : 'google-chrome');
const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'gitpane-theme-contrast-'));
const css = fs.readFileSync(path.join(extension, 'src/webview/styles.css'), 'utf8');
const theme = fs.readFileSync(path.join(extension, 'src/theme-webview.js'), 'utf8');
const view = fs.readFileSync(path.join(extension, 'src/webview/view.js'), 'utf8');
const graphCss = fs.readFileSync(path.join(extension, 'src/graph/styles.css'), 'utf8');
const graphView = fs.readFileSync(path.join(extension, 'src/graph/view.js'), 'utf8');
const scenario = String.raw`
(async () => {
  const checks = [];
  const check = (name, condition) => { if (!condition) throw new Error(name); checks.push(name); };
  const tick = () => new Promise(resolve => setTimeout(resolve, 30));
  const luminance = color => color.match(/\d+/g).slice(0, 3).map(Number).map(value => {
    const c = value / 255;
    return c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
  }).reduce((sum, value, index) => sum + value * [0.2126, 0.7152, 0.0722][index], 0);
  const contrast = (a, b) => {
    const x = luminance(a), y = luminance(b);
    return (Math.max(x, y) + 0.05) / (Math.min(x, y) + 0.05);
  };
  try {
    const forced = matchMedia('(forced-colors: active)').matches;
    check('forced colors mode matches requested environment', forced === EXPECT_FORCED);
    window.postMessage({ type: 'snapshot', errors: [], repositories: [{
      id: 'repo', name: 'repo', branch: 'feature', dirty: true, stashCount: 1,
      stashes: [{ id: 'stash', index: 0, message: 'saved' }], groups: []
    }], theme: { name: 'custom', values: {
      'repo-list-stash': 'rgb(240, 40, 120)', 'repo-list-branch': 'rgb(20, 180, 210)',
      'repo-list-dirty-marker': 'rgb(220, 180, 0)', 'repo-list-repo-name': 'rgb(140, 80, 240)'
    }, lanes: ['rgb(20, 180, 210)'], authors: ['rgb(220, 100, 0)'] } }, '*');
    await tick();
    const treeitem = document.querySelector('[role="treeitem"]');
    treeitem.focus();
    await tick();
    const row = treeitem.querySelector('.repo-row');
    const background = getComputedStyle(row).backgroundColor;
    for (const selector of ['.name', '.branch', '.dirty', '.stash-action', '.graph-action', '.menu-action']) {
      const node = row.querySelector(selector);
      check('selected ' + selector + ' is readable', !!node && contrast(getComputedStyle(node).color, background) >= 4.5);
    }
    check('selected stash button uses selection foreground', getComputedStyle(row.querySelector('.stash-action')).color === getComputedStyle(row).color);
    if (!forced) check('VS Code high contrast uses inherited semantic color with nonempty palettes',
      window.GitpaneTheme.lane(0) === 'currentColor' && window.GitpaneTheme.author('Andres') === 'currentColor');
    document.body.dataset.result = btoa(JSON.stringify({ ok: true, checks, forced }));
  } catch (error) {
    document.body.dataset.result = btoa(JSON.stringify({ ok: false, checks, error: error.stack || String(error) }));
  }
})();`;

const graphScenario = String.raw`
(async () => {
  const checks = [];
  const check = (name, condition) => { if (!condition) throw new Error(name); checks.push(name); };
  const tick = () => new Promise(resolve => setTimeout(resolve, 30));
  const luminance = color => color.match(/\d+/g).slice(0, 3).map(Number).map(value => {
    const c = value / 255;
    return c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
  }).reduce((sum, value, index) => sum + value * [0.2126, 0.7152, 0.0722][index], 0);
  const contrast = (a, b) => {
    const x = luminance(a), y = luminance(b);
    return (Math.max(x, y) + 0.05) / (Math.min(x, y) + 0.05);
  };
  try {
    check('forced colors mode matches requested environment', matchMedia('(forced-colors: active)').matches === EXPECT_FORCED);
    window.postMessage({ type: 'theme', theme: { name: 'custom', values: {
      'graph-head-marker': 'rgb(230, 40, 120)', 'graph-commit-message': 'rgb(160, 80, 240)',
      'graph-commit-id': 'rgb(220, 180, 0)' }, lanes: ['rgb(20, 180, 210)'], authors: ['rgb(220, 100, 0)'] } }, '*');
    window.postMessage({ type: 'graph', loading: false, rows: [{ oid: 'abc', short_id: 'abc', message: 'commit',
      author: 'Andres', lanes: ['commit'], horizontal_spans: [], labels: [{ name: 'main', is_head: true }] }] }, '*');
    await tick();
    if (!EXPECT_FORCED) {
      const unselected = document.querySelector('.commit');
      const foreground = getComputedStyle(document.body).color;
      check('unselected graph lane uses high-contrast foreground', getComputedStyle(unselected.querySelector('.lane span')).color === foreground);
      check('unselected graph author uses high-contrast foreground', getComputedStyle(unselected.querySelector('.author')).color === foreground);
    }
    document.querySelector('.commit').click();
    const row = document.querySelector('.commit');
    const background = getComputedStyle(row).backgroundColor;
    for (const selector of ['.lane span', '.hash', '.head', '.summary', '.author']) {
      const node = row.querySelector(selector);
      check('selected graph ' + selector + ' is readable', !!node && contrast(getComputedStyle(node).color, background) >= 4.5);
    }
    document.body.dataset.result = btoa(JSON.stringify({ ok: true, checks }));
  } catch (error) {
    document.body.dataset.result = btoa(JSON.stringify({ ok: false, checks, error: error.stack || String(error) }));
  }
})();`;

function run(mode) {
  const forced = mode === 'forced';
  const light = mode === 'high-contrast-light';
  const bodyClass = forced ? 'vscode-dark' : `vscode-${mode}`;
  const bg = light ? '#fff' : '#000';
  const fg = light ? '#000' : '#fff';
  const colors = `--vscode-sideBar-background:${bg};--vscode-sideBar-foreground:${fg};` +
    '--vscode-list-activeSelectionBackground:#173a72;--vscode-list-activeSelectionForeground:#fff;' +
    '--vscode-list-inactiveSelectionBackground:#173a72;--vscode-list-inactiveSelectionForeground:#fff;--vscode-focusBorder:#fff;';
  const html = `<!DOCTYPE html><html lang="en"><head><meta charset="utf-8">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'nonce-test'; script-src 'nonce-test'">
<style nonce="test">:root {${colors}}\n${css}</style></head><body class="${bodyClass}">
<div id="messages" aria-live="polite"></div><div id="repositories" role="tree" aria-label="Gitpane repositories"></div>
<script nonce="test">window.acquireVsCodeApi=()=>({postMessage(){},getState(){return {}},setState(){}});</script>
<script nonce="test">${theme}</script><script nonce="test">${view}</script>
<script nonce="test">${scenario.replace('EXPECT_FORCED', String(forced))}</script></body></html>`;
  const fixture = path.join(temporary, `${mode}.html`);
  fs.writeFileSync(fixture, html);
  const args = ['--headless', '--disable-gpu', '--disable-background-networking', '--no-first-run', '--disable-extensions',
    `--user-data-dir=${path.join(temporary, `profile-${mode}`)}`, '--window-size=500,650', '--virtual-time-budget=4000',
    ...(forced ? ['--force-high-contrast'] : []), '--dump-dom', pathToFileURL(fixture).href];
  const result = spawnSync(browser, args, { encoding: 'utf8', timeout: 30000 });
  if (result.error || result.status !== 0) throw new Error(`Chrome ${mode} failed: ${result.error?.stack || result.stderr}`);
  const encoded = result.stdout.match(/data-result="([^"]+)"/)?.[1];
  if (!encoded) throw new Error(`Chrome ${mode} did not report results:\n${result.stdout}\n${result.stderr}`);
  const reported = JSON.parse(Buffer.from(encoded, 'base64').toString('utf8'));
  if (!reported.ok) throw new Error(`Chrome ${mode} contrast failed: ${reported.error}`);
  console.log(`${mode}: ${reported.checks.length} contrast checks passed`);
}

function runGraph(mode) {
  const forced = mode === 'forced';
  const light = mode === 'high-contrast-light';
  const bodyClass = forced ? 'vscode-dark' : `vscode-${mode}`;
  const colors = `--vscode-editor-background:${light ? '#fff' : '#000'};--vscode-editor-foreground:${light ? '#000' : '#fff'};` +
    '--vscode-list-inactiveSelectionBackground:#173a72;--vscode-list-inactiveSelectionForeground:#fff;--vscode-focusBorder:#fff;';
  const html = `<!DOCTYPE html><html lang="en"><head><meta charset="utf-8">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'nonce-test'; script-src 'nonce-test'">
<style nonce="test">:root {${colors}}\n${graphCss}</style></head><body class="${bodyClass}">
<input id="search"><span id="search-status"></span><button id="search-prev"></button><button id="search-next"></button>
<span id="graph-status"></span><div id="theme-status"></div><input id="first-parent" type="checkbox"><button id="filters"></button>
<button id="refresh"></button><button id="collapse"></button><button id="expand-all"></button>
<div id="graph" role="listbox" tabindex="0"></div><h2 id="files-heading"></h2><div id="files-status"></div>
<ul id="files"></ul><input id="file-search"><script nonce="test">window.acquireVsCodeApi=()=>({postMessage(){},getState(){return {}},setState(){}});</script>
<script nonce="test">${theme}</script><script nonce="test">${graphView}</script>
<script nonce="test">${graphScenario.replaceAll('EXPECT_FORCED', String(forced))}</script></body></html>`;
  const fixture = path.join(temporary, `graph-${mode}.html`);
  fs.writeFileSync(fixture, html);
  const args = ['--headless', '--disable-gpu', '--disable-background-networking', '--no-first-run', '--disable-extensions',
    `--user-data-dir=${path.join(temporary, `graph-profile-${mode}`)}`, '--window-size=700,650', '--virtual-time-budget=4000',
    ...(forced ? ['--force-high-contrast'] : []), '--dump-dom', pathToFileURL(fixture).href];
  const result = spawnSync(browser, args, { encoding: 'utf8', timeout: 30000 });
  if (result.error || result.status !== 0) throw new Error(`Chrome graph ${mode} failed: ${result.error?.stack || result.stderr}`);
  const encoded = result.stdout.match(/data-result="([^"]+)"/)?.[1];
  if (!encoded) throw new Error(`Chrome graph ${mode} did not report results:\n${result.stdout}\n${result.stderr}`);
  const reported = JSON.parse(Buffer.from(encoded, 'base64').toString('utf8'));
  if (!reported.ok) throw new Error(`Chrome graph ${mode} contrast failed: ${reported.error}`);
  console.log(`graph ${mode}: ${reported.checks.length} contrast checks passed`);
}

try {
  for (const mode of ['high-contrast', 'high-contrast-light', 'forced']) { run(mode); runGraph(mode); }
} catch (error) {
  console.error(error.stack || String(error));
  process.exitCode = 1;
} finally {
  fs.rmSync(temporary, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });
}
