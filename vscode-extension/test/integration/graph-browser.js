'use strict';

const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const { pathToFileURL } = require('node:url');

const extension = path.resolve(__dirname, '../..');
const browser = process.env.CHROME_EXECUTABLE || (process.platform === 'darwin'
  ? '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome' : 'google-chrome');
const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'gitpane-graph-test-'));
const stylesheet = fs.readFileSync(path.join(extension, 'src/graph/styles.css'), 'utf8');
const client = fs.readFileSync(path.join(extension, 'src/graph/view.js'), 'utf8');
const themeClient = fs.readFileSync(path.join(extension, 'src/theme-webview.js'), 'utf8');
const snapshot = {
  type: 'graph', loading: false, firstParent: false, limit: 2,
  filters: { branches: [], authors: null, refs: { local: true, remote: false, tags: true, stashes: true } },
  rows: [
    { oid: 'abcdef1234', short_id: 'abcdef1', message: 'Merge changes', author: 'Andres',
      time: 1760000000, is_merge: true, commit_col: 0,
      lanes: ['commit', 'cross_horizontal', 'merge_left'], horizontal_spans: [[0, 2, 1]],
      parent_oids: ['999', '888'], labels: [{ name: 'feature/test', is_head: true }],
      diff_stat: { additions: 4, deletions: 2 } },
    { oid: '1234567890', short_id: '1234567', message: '</script><img src=x onerror="window.injected=true">',
      author: 'Another Author', time: 1760000001, is_merge: false, commit_col: 1,
      lanes: ['straight', 'commit'], horizontal_spans: [], parent_oids: [], labels: [], diff_stat: null, segment_id: 'segment-1' }
  ]
};
const encodedSnapshot = Buffer.from(JSON.stringify(snapshot)).toString('base64');
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
  const snapshot = JSON.parse(atob('SNAPSHOT_BASE64'));
  try {
    check('webview requests initial graph', window.posted.some(message => message.type === 'ready'));
    window.postMessage(snapshot, '*');
    await tick();
    const graph = document.getElementById('graph');
    const options = () => [...graph.querySelectorAll('[role="option"]')];
    check('graph is keyboard accessible', graph.getAttribute('role') === 'listbox' && graph.tabIndex === 0);
    check('service rows shown', options().length === 2);
    document.documentElement.style.setProperty('--vscode-terminal-ansiRed', 'rgb(255, 0, 0)');
    const theme = { name: 'custom', values: { 'graph-head-marker': 'rgb(255, 0, 0)',
      'graph-tag-label': 'var(--vscode-terminal-ansiRed)', 'graph-commit-message': 'rgb(0, 180, 180)',
      'graph-merge-message': 'rgb(255, 100, 0)', 'graph-commit-files-path': 'rgb(160, 80, 240)' },
      lanes: ['rgb(255, 0, 0)', 'rgb(0, 255, 0)', 'rgb(0, 0, 255)'],
      authors: ['rgb(255, 100, 0)', 'rgb(0, 180, 180)', 'rgb(160, 80, 240)'] };
    window.postMessage({ type: 'theme', theme }, '*');
    await tick();
    const originalClass = document.body.className;
    document.body.className = 'vscode-high-contrast-light';
    await tick();
    check('high contrast uses VS Code foreground', window.GitpaneTheme.lane(0) === 'currentColor' && window.GitpaneTheme.author('Andres') === 'currentColor');
    document.body.className = originalClass;
    await tick();
    check('theme applies semantic CSS variables under CSP', !!document.documentElement.style.getPropertyValue('--gitpane-graph-head-marker'));
    check('merge and regular commit text use distinct theme colors',
      getComputedStyle(options()[0].querySelector('.summary')).color !== getComputedStyle(options()[1].querySelector('.summary')).color);
    const previousTagColor = document.documentElement.style.getPropertyValue('--gitpane-graph-tag-label');
    document.documentElement.style.setProperty('--vscode-terminal-ansiRed', 'rgb(0, 200, 0)');
    window.postMessage({ type: 'theme', theme: { ...theme, appearance: 1 } }, '*');
    await tick();
    check('same-class VS Code theme changes reapply ANSI colors',
      previousTagColor !== document.documentElement.style.getPropertyValue('--gitpane-graph-tag-label'));
    check('three-color lane palette follows native six-column mapping', window.GitpaneTheme.lane(0) === window.GitpaneTheme.lane(6) &&
      window.GitpaneTheme.lane(1) === window.GitpaneTheme.lane(4) && window.GitpaneTheme.lane(0) !== window.GitpaneTheme.lane(1));
    const firstAuthorColor = window.GitpaneTheme.author('Another Author');
    const andresColor = window.GitpaneTheme.author('Andres');
    window.postMessage({ type: 'theme', theme: { ...theme, authors: [theme.authors[1], theme.authors[0], theme.authors[2]] } }, '*');
    await tick();
    check('author palette follows native FNV-1a mapping', window.GitpaneTheme.author('Andres') === firstAuthorColor &&
      window.GitpaneTheme.author('Another Author') === andresColor);
    window.postMessage({ type: 'theme', theme }, '*');
    await tick();
    check('palette colors meet contrast', contrast(getComputedStyle(options()[0].querySelector('.head')).color,
      getComputedStyle(document.body).backgroundColor) >= 4.5);
    const oldColor = document.documentElement.style.getPropertyValue('--gitpane-graph-head-marker');
    window.postMessage({ type: 'theme', theme: { ...theme, values: { 'graph-head-marker': 'red; display:none' } } }, '*');
    await tick();
    check('malformed theme cannot inject CSS', document.documentElement.style.getPropertyValue('--gitpane-graph-head-marker') === oldColor);
    window.postMessage({ type: 'theme', theme: { ...theme, lanes: [], authors: [] } }, '*');
    await tick();
    check('empty palettes use inherited color', window.GitpaneTheme.lane(0) === 'currentColor' && window.GitpaneTheme.author('Andres') === 'currentColor');
    window.postMessage({ type: 'theme', theme }, '*');
    await tick();
    check('commit limit disclosed', document.getElementById('graph-status').textContent === 'Showing up to 2 commits.');
    check('active filters disclosed accessibly', document.getElementById('filters').getAttribute('aria-label') === 'Filter commit graph, 2 active');
    document.getElementById('filters').click();
    check('filter button opens picker', window.posted.some(message => message.type === 'openFilters'));
    document.getElementById('refresh').click();
    check('refresh requests a new graph', window.posted.some(message => message.type === 'refresh'));
    check('canonical lane glyphs and continuation shown', options()[0].querySelector('.lanes').textContent === '●─┼─╯ ');
    check('canonical lane color used', options()[0].querySelectorAll('.lane')[1].firstChild.className === 'lane-1');
    check('labels and stats visible', options()[0].textContent.includes('feature/test') && options()[0].textContent.includes('+4') && options()[0].textContent.includes('−2'));
    const labelSnapshot = structuredClone(snapshot);
    labelSnapshot.labelMaxLen = 9;
    labelSnapshot.rows[0].labels[0].name = 'feature/😀/mixed';
    window.postMessage(labelSnapshot, '*');
    await tick();
    const label = () => options()[0].querySelector('.head');
    check('label limit preserves complete Unicode characters', label().textContent === '* feature/😀…');
    check('full label remains accessible', label().title === 'feature/😀/mixed' && options()[0].getAttribute('aria-label').includes('feature/😀/mixed'));
    const labelSearch = document.getElementById('search');
    labelSearch.value = 'mixed';
    labelSearch.dispatchEvent(new Event('input', { bubbles: true }));
    check('search matches the full label', !options()[0].classList.contains('nonmatch'));
    labelSnapshot.labelMaxLen = 0;
    window.postMessage(labelSnapshot, '*');
    await tick();
    check('zero label limit shows ellipsis', label().textContent === '* …');
    labelSearch.value = '';
    labelSearch.dispatchEvent(new Event('input', { bubbles: true }));
    window.postMessage(snapshot, '*');
    await tick();
    check('hostile commit text rendered safely', document.body.textContent.includes(snapshot.rows[1].message) && !document.querySelector('img') && !window.injected);
    const search = document.getElementById('search');
    search.value = 'Another Author';
    search.dispatchEvent(new Event('input', { bubbles: true }));
    check('search keeps all graph topology rows', options().length === 2 && options()[0].classList.contains('nonmatch') && !options()[1].classList.contains('nonmatch'));
    check('search announces matches', document.getElementById('search-status').textContent === '1 match');
    check('search persists', window.savedState.search === 'Another Author');
    search.value = '123';
    search.dispatchEvent(new Event('input', { bubbles: true }));
    search.focus();
    document.getElementById('search-next').click();
    check('next match selects first match without moving focus', document.activeElement === search && options()[0].getAttribute('aria-selected') === 'true');
    search.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
    check('enter selects next commit match', options()[1].getAttribute('aria-selected') === 'true');
    search.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
    check('next commit match wraps', options()[0].getAttribute('aria-selected') === 'true');
    search.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', shiftKey: true, bubbles: true }));
    check('previous commit match wraps', options()[1].getAttribute('aria-selected') === 'true');
    search.value = 'not-found';
    search.dispatchEvent(new Event('input', { bubbles: true }));
    check('zero matches disable navigation', document.getElementById('search-next').disabled && document.getElementById('search-prev').disabled);
    search.value = 'Another Author';
    search.dispatchEvent(new Event('input', { bubbles: true }));
    options()[0].click();
    check('click selects commit', window.posted.some(message => message.type === 'selectCommit' && message.oid === 'abcdef1234'));
    check('file loading shown', document.getElementById('files-status').textContent.includes('Loading'));
    check('commit actions wait for verified selection', document.getElementById('commit-actions').disabled);
    window.postMessage({ type: 'commitFiles', oid: '1234567890', files: [{ id: 'stale', path: 'stale.txt', status: 'M' }] }, '*');
    await tick();
    check('stale commit response ignored', !document.getElementById('files').textContent.includes('stale.txt'));
    window.postMessage({ type: 'commitFiles', oid: 'abcdef1234', files: [
      { id: 'safe-id', path: '<img src=x>', status: 'M' }, { id: 'source-id', path: 'src/Server.rs', status: 'A' },
      { id: 'docs-id', path: 'docs/readme.md', status: 'M' }
    ] }, '*');
    await tick();
    check('commit actions are available for a selected commit', !document.getElementById('commit-actions').disabled);
    document.getElementById('commit-actions').click();
    check('commit actions use the selected OID', window.posted.at(-1).type === 'commitActions' && window.posted.at(-1).oid === 'abcdef1234');
    graph.dispatchEvent(new KeyboardEvent('keydown', { key: 'y', bubbles: true }));
    check('graph keyboard copies selected commit summary', window.posted.at(-1).type === 'copyCommitSummary' && window.posted.at(-1).oid === 'abcdef1234');
    graph.dispatchEvent(new KeyboardEvent('keydown', { key: 'F10', shiftKey: true, bubbles: true }));
    check('keyboard opens commit actions', window.posted.at(-1).type === 'commitActions' && window.posted.at(-1).oid === 'abcdef1234');
    options()[1].dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true }));
    check('right click waits for new commit selection', window.posted.at(-1).type === 'selectCommit' && window.posted.at(-1).oid === '1234567890');
    window.postMessage({ type: 'commitFiles', oid: '1234567890', files: [], loading: true }, '*');
    await tick();
    check('right click opens actions for the verified commit', window.posted.at(-1).type === 'commitActions' && window.posted.at(-1).oid === '1234567890');
    options()[0].click();
    window.postMessage({ type: 'commitFiles', oid: 'abcdef1234', files: [
      { id: 'safe-id', path: '<img src=x>', status: 'M' }, { id: 'source-id', path: 'src/Server.rs', status: 'A' },
      { id: 'docs-id', path: 'docs/readme.md', status: 'M' }
    ] }, '*');
    await tick();
    check('commit file path rendered safely', document.getElementById('files').textContent.includes('<img src=x>') && !document.querySelector('img'));
    check('commit file path uses native semantic color', getComputedStyle(document.querySelector('.file-path')).color ===
      document.documentElement.style.getPropertyValue('--gitpane-graph-commit-files-path'));
    document.querySelector('.file').click();
    check('file opens diff', window.posted.some(message => message.type === 'openCommitFile' && message.id === 'safe-id'));
    const selectedBeforeRefresh = window.posted.filter(message => message.type === 'selectCommit').length;
    window.postMessage({ ...snapshot, loading: true }, '*');
    await tick();
    options()[1].click();
    graph.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowDown', bubbles: true }));
    search.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
    check('retained rows cannot change selection during refresh', options()[0].getAttribute('aria-selected') === 'true' &&
      window.posted.filter(message => message.type === 'selectCommit').length === selectedBeforeRefresh &&
      graph.getAttribute('aria-disabled') === 'true');
    check('refresh removes stale file actions', !document.querySelector('.file'));
    check('refresh disables commit actions', document.getElementById('commit-actions').disabled);
    window.postMessage({ ...snapshot, error: 'Refresh failed' }, '*');
    await tick();
    options()[1].click();
    check('retained rows remain disabled on refresh error', options()[0].getAttribute('aria-selected') === 'true' &&
      window.posted.filter(message => message.type === 'selectCommit').length === selectedBeforeRefresh);
    window.postMessage(snapshot, '*');
    await tick();
    check('selection remains on the same commit after recovery', options()[0].getAttribute('aria-selected') === 'true' &&
      graph.getAttribute('aria-disabled') === 'false');
    window.postMessage({ type: 'commitFiles', oid: 'abcdef1234', files: [
      { id: 'safe-id', path: '<img src=x>', status: 'M' }, { id: 'source-id', path: 'src/Server.rs', status: 'A' },
      { id: 'docs-id', path: 'docs/readme.md', status: 'M' }
    ] }, '*');
    await tick();
    const fileSearch = document.getElementById('file-search');
    fileSearch.value = 'SERVER';
    fileSearch.dispatchEvent(new Event('input', { bubbles: true }));
    check('file search is case-insensitive', document.querySelectorAll('.file').length === 1 && document.querySelector('.file').textContent.includes('src/Server.rs'));
    fileSearch.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
    check('file search keyboard focuses matching file', document.activeElement === document.querySelector('.file'));
    document.activeElement.dispatchEvent(new KeyboardEvent('keydown', { key: 'y', bubbles: true }));
    check('file keyboard copies the focused path', window.posted.at(-1).type === 'copyCommitFile' && window.posted.at(-1).id === 'source-id');
    document.activeElement.click();
    check('filtered file opens the selected diff', window.posted.some(message => message.type === 'openCommitFile' && message.id === 'source-id'));
    fileSearch.value = 'missing';
    fileSearch.dispatchEvent(new Event('input', { bubbles: true }));
    check('file search reports no matches', !document.querySelector('.file') && document.getElementById('files-status').textContent === 'No matching files.');
    const firstParent = document.getElementById('first-parent');
    firstParent.click();
    check('first-parent toggle requests new graph', window.posted.some(message => message.type === 'firstParent' && message.value === true));
    graph.focus();
    graph.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowDown', bubbles: true }));
    check('keyboard selects next commit', window.posted.some(message => message.type === 'selectCommit' && message.oid === '1234567890'));
    check('file search resets when commit changes', fileSearch.value === '' && fileSearch.disabled);
    window.postMessage({ type: 'commitFiles', oid: '1234567890', error: 'Could not load files <img>' }, '*');
    await tick();
    check('file errors are visible and safe', document.getElementById('files-status').textContent.includes('Could not load files <img>') && !document.querySelector('img'));
    const collapse = document.getElementById('collapse');
    check('selected segment offers collapse', !collapse.disabled && collapse.textContent === 'Collapse branch');
    collapse.click();
    check('collapse requests opaque segment', window.posted.some(message => message.type === 'toggleCollapse' && message.id === 'segment-1'));
    window.postMessage({ type: 'graph', rows: [], loading: true }, '*');
    await tick();
    const collapsed = JSON.parse(JSON.stringify(snapshot));
    Object.assign(collapsed.rows[1], { message: '▸ topic (3 commits)', short_id: '', collapsed: { id: 'segment-1', count: 3 } });
    collapsed.collapsedCount = 1;
    window.postMessage(collapsed, '*');
    await tick();
    check('collapsed segment is accessible', options()[1].getAttribute('aria-label').includes('collapsed branch') && collapse.textContent === 'Expand branch');
    const previousSelections = window.posted.filter(message => message.type === 'selectCommit').length;
    options()[1].click();
    check('collapsed placeholder cannot request commit files', window.posted.filter(message => message.type === 'selectCommit').length === previousSelections &&
      document.getElementById('files-status').textContent.includes('Expand the branch'));
    check('collapsed placeholder invalidates earlier commit selections', window.posted.at(-1).type === 'selectCollapsed' && window.posted.at(-1).oid === '1234567890');
    check('collapsed placeholder disables commit actions', document.getElementById('commit-actions').disabled);
    document.getElementById('expand-all').click();
    check('expand all requests restoration', window.posted.some(message => message.type === 'expandAll'));
    window.postMessage({ type: 'graph', rows: [], loading: true }, '*');
    await tick();
    window.postMessage(snapshot, '*');
    await tick();
    check('expanded selection loads a real commit', window.posted.filter(message => message.type === 'selectCommit').length === previousSelections + 1 &&
      window.posted.at(-1).type === 'selectCommit' && window.posted.at(-1).oid === '1234567890');
    window.postMessage({ type: 'graph', rows: [], loading: false, error: 'Cannot load graph', firstParent: true }, '*');
    await tick();
    check('graph errors visible', document.getElementById('graph-status').textContent === 'Cannot load graph');
    check('first-parent state follows backend', firstParent.checked);
    window.postMessage(snapshot, '*');
    await tick();
    options()[0].click();
    window.postMessage({ type: 'commitFiles', oid: 'abcdef1234', files: [{ id: 'safe-id', path: 'src/server.rs', status: 'M' }] }, '*');
    await tick();
    document.body.dataset.result = btoa(JSON.stringify({ ok: true, checks }));
  } catch (error) {
    document.body.dataset.result = btoa(JSON.stringify({ ok: false, checks, error: error.stack || String(error) }));
  }
})();`;

function documentFor(theme) {
  const light = theme === 'light';
  const colors = light
    ? '--vscode-editor-background:#fff;--vscode-editor-foreground:#333;--vscode-list-inactiveSelectionBackground:#ddd;--vscode-focusBorder:#007acc;'
    : '--vscode-editor-background:#1e1e1e;--vscode-editor-foreground:#ddd;--vscode-list-inactiveSelectionBackground:#383838;--vscode-focusBorder:#007acc;';
  return `<!DOCTYPE html><html lang="en"><head><meta charset="utf-8"><meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'nonce-gitpane-test'; script-src 'nonce-gitpane-test'"><style nonce="gitpane-test">:root {${colors}}\n${stylesheet}</style></head>
<body class="vscode-${theme}"><header class="toolbar"><label class="search-label" for="search">Search commits</label><input id="search" type="search" placeholder="Search commits" autocomplete="off" spellcheck="false"><button id="search-prev" type="button" title="Previous match" aria-label="Previous commit match">↑</button><button id="search-next" type="button" title="Next match" aria-label="Next commit match">↓</button><label class="first-parent"><input id="first-parent" type="checkbox"> First parent</label><button id="filters" type="button" aria-label="Filter commit graph">Filters</button><button id="collapse" type="button" disabled>Collapse branch</button><button id="expand-all" type="button" disabled>Expand all</button><button id="commit-actions" type="button" disabled>Commit actions</button><button id="refresh" type="button" title="Refresh commit graph" aria-label="Refresh commit graph">↻</button></header>
<div id="search-status" class="search-status" role="status" aria-live="polite"></div><div id="graph-status" class="message" role="status" aria-live="polite"></div><div id="theme-status" role="alert"></div><div id="graph" class="graph" role="listbox" aria-label="Commit graph" tabindex="0"></div>
<section class="details" aria-label="Commit files"><h2 id="files-heading">Commit files</h2><label class="search-label" for="file-search">Search commit files</label><input id="file-search" type="search" placeholder="Search files" autocomplete="off" spellcheck="false" disabled><div id="files-status" class="message" role="status" aria-live="polite">Select a commit to see its files.</div><ul id="files" class="files" aria-labelledby="files-heading"></ul></section>
<script nonce="gitpane-test">window.posted=[];window.savedState={};window.acquireVsCodeApi=()=>({postMessage:m=>window.posted.push(m),getState:()=>window.savedState,setState:s=>window.savedState=s});</script>
<script nonce="gitpane-test">${themeClient}</script><script nonce="gitpane-test">${client}</script><script nonce="gitpane-test">${scenario.replace('SNAPSHOT_BASE64', encodedSnapshot)}</script></body></html>`;
}

function run(theme) {
  const fixture = path.join(temporary, `${theme}.html`);
  const profile = path.join(temporary, `profile-${theme}`);
  const screenshot = path.join(os.tmpdir(), `gitpane-graph-${theme}.png`);
  fs.writeFileSync(fixture, documentFor(theme));
  const args = ['--headless', '--disable-gpu', '--disable-background-networking', '--no-first-run',
    '--disable-extensions', `--user-data-dir=${profile}`, '--window-size=850,650', '--virtual-time-budget=4000'];
  const dom = spawnSync(browser, [...args, '--dump-dom', pathToFileURL(fixture).href], { encoding: 'utf8', timeout: 30000 });
  if (dom.error || dom.status !== 0) throw new Error(`Chrome ${theme} test failed: ${dom.error?.stack || dom.stderr}`);
  const encoded = dom.stdout.match(/data-result="([^"]+)"/)?.[1];
  if (!encoded) throw new Error(`Chrome ${theme} did not report results:\n${dom.stdout}\n${dom.stderr}`);
  const result = JSON.parse(Buffer.from(encoded, 'base64').toString('utf8'));
  if (!result.ok) throw new Error(`Chrome ${theme} graph behavior failed: ${result.error}`);
  const image = spawnSync(browser, [...args, `--screenshot=${screenshot}`, pathToFileURL(fixture).href], { encoding: 'utf8', timeout: 30000 });
  if (image.error || image.status !== 0 || !fs.existsSync(screenshot)) throw new Error(`Chrome ${theme} screenshot failed: ${image.error?.stack || image.stderr}`);
  console.log(`${theme}: ${result.checks.length} graph checks passed; screenshot ${screenshot}`);
}

try {
  run('dark');
  run('light');
} catch (error) {
  console.error(error.stack || String(error));
  process.exitCode = 1;
} finally {
  fs.rmSync(temporary, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });
}
